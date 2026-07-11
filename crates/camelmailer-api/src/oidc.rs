//! OpenID Connect single sign-on (`/api/v2/auth/oidc/*`).
//!
//! Authorization-code flow with PKCE (S256) and a server-side state store,
//! validated end to end: discovery via
//! `{issuer}/.well-known/openid-configuration`, ID-token signatures against
//! the provider's JWKS (RS256/RS384/RS512), `iss`/`aud`/`exp` and the
//! `nonce` bound to the in-flight login. Works with any spec-compliant
//! provider (Okta, Entra ID, Google Workspace, Keycloak, Authentik, …).
//!
//! Accounts are linked by the configured `uid_field` claim (default `sub`);
//! on first SSO login an existing account with the same email address is
//! linked, otherwise one is provisioned (when `oidc.auto_provision` is on
//! and the address passes `allowed_email_domains`).

use axum::extract::{Query, Request, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use camelmailer_core::auth::{self, NewAuthEvent};
use camelmailer_core::{AuthStore, StoreError};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;

use crate::app::{
    render_error, render_success, timing_middleware, ApiResponse, ApiState, RequestStart,
};
use crate::auth_api::{client_ip, issue_session, user_json};

const STATE_TTL_MINUTES: i64 = 10;

fn sso_error(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(start, StatusCode::UNPROCESSABLE_ENTITY, "SSOError", message)
}

fn disabled(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::NOT_FOUND,
        "SSODisabled",
        "OpenID Connect single sign-on is not enabled on this instance",
    )
}

/// The subset of the discovery document we need.
#[derive(Debug, Clone, Deserialize)]
struct Discovery {
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    issuer: String,
}

async fn fetch_discovery(issuer: &str) -> Result<Discovery, String> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let response = reqwest::get(&url)
        .await
        .map_err(|error| format!("could not reach the identity provider: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "identity provider discovery failed with HTTP {}",
            response.status()
        ));
    }
    response
        .json::<Discovery>()
        .await
        .map_err(|error| format!("invalid discovery document: {error}"))
}

/// The redirect URI registered with the identity provider.
fn redirect_uri(state: &ApiState) -> String {
    format!(
        "{}://{}/api/v2/auth/oidc/callback",
        state.config.camelmailer.web_protocol, state.config.camelmailer.web_hostname
    )
}

// -------------------------------------------------------------- /start

/// `GET /api/v2/auth/oidc/start` — begin the code flow. Responds with a
/// redirect to the provider's authorization endpoint (or the URL as JSON
/// when requested with `Accept: application/json`).
async fn oidc_start(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
) -> Response {
    if !state.config.oidc.enabled {
        return disabled(Some(&start.0)).into_response();
    }
    let Some(store) = state.auth_store.clone() else {
        return sso_error(Some(&start.0), "accounts require persistent storage").into_response();
    };
    let discovery = match fetch_discovery(&state.config.oidc.issuer).await {
        Ok(discovery) => discovery,
        Err(message) => return sso_error(Some(&start.0), &message).into_response(),
    };

    let login_state = auth::generate_auth_token();
    let nonce = auth::generate_auth_token();
    let pkce_verifier = auth::generate_auth_token();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce_verifier.as_bytes()));
    if let Err(error) = store
        .create_oidc_state(
            &login_state,
            &pkce_verifier,
            &nonce,
            Utc::now() + Duration::minutes(STATE_TTL_MINUTES),
        )
        .await
    {
        return crate::app::render_store_error(Some(&start.0), error).into_response();
    }

    let scopes = state.config.oidc.scopes.join(" ");
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        discovery.authorization_endpoint,
        urlencode(state.config.oidc.identifier.as_deref().unwrap_or("")),
        urlencode(&redirect_uri(&state)),
        urlencode(&scopes),
        login_state,
        nonce,
        challenge,
    );

    let wants_json = headers
        .get("accept")
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("application/json"))
        .unwrap_or(false);
    if wants_json {
        render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "authorization_url": url }),
        )
        .into_response()
    } else {
        Redirect::temporary(&url).into_response()
    }
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

// ----------------------------------------------------------- /callback

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Debug, Deserialize)]
struct Jwk {
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    kty: String,
    n: Option<String>,
    e: Option<String>,
}

async fn oidc_callback(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    if !state.config.oidc.enabled {
        return disabled(Some(&start.0)).into_response();
    }
    let Some(store) = state.auth_store.clone() else {
        return sso_error(Some(&start.0), "accounts require persistent storage").into_response();
    };
    if let Some(error) = query.error {
        let description = query.error_description.unwrap_or_default();
        return sso_error(
            Some(&start.0),
            &format!("the identity provider reported: {error} {description}"),
        )
        .into_response();
    }
    let (Some(code), Some(login_state)) = (query.code, query.state) else {
        return sso_error(Some(&start.0), "missing code or state parameter").into_response();
    };

    // Redeem the state — single use, expiring.
    let (pkce_verifier, nonce) = match store.consume_oidc_state(&login_state, Utc::now()).await {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return sso_error(Some(&start.0), "the login state is invalid or has expired")
                .into_response()
        }
        Err(error) => return crate::app::render_store_error(Some(&start.0), error).into_response(),
    };

    let discovery = match fetch_discovery(&state.config.oidc.issuer).await {
        Ok(discovery) => discovery,
        Err(message) => return sso_error(Some(&start.0), &message).into_response(),
    };

    // Exchange the code.
    let mut form: Vec<(&str, String)> = vec![
        ("grant_type", "authorization_code".into()),
        ("code", code),
        ("redirect_uri", redirect_uri(&state)),
        (
            "client_id",
            state.config.oidc.identifier.clone().unwrap_or_default(),
        ),
        ("code_verifier", pkce_verifier),
    ];
    if let Some(secret) = state.config.oidc.secret.clone() {
        form.push(("client_secret", secret));
    }
    let token_response = match reqwest::Client::new()
        .post(&discovery.token_endpoint)
        .form(&form)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            match response.json::<TokenResponse>().await {
                Ok(token_response) => token_response,
                Err(error) => {
                    return sso_error(Some(&start.0), &format!("invalid token response: {error}"))
                        .into_response()
                }
            }
        }
        Ok(response) => {
            return sso_error(
                Some(&start.0),
                &format!("token exchange failed with HTTP {}", response.status()),
            )
            .into_response()
        }
        Err(error) => {
            return sso_error(Some(&start.0), &format!("token exchange failed: {error}"))
                .into_response()
        }
    };
    let Some(id_token) = token_response.id_token else {
        return sso_error(Some(&start.0), "the token response carried no id_token").into_response();
    };

    // Validate the ID token against the provider's JWKS.
    let claims = match validate_id_token(&state, &discovery, &id_token, &nonce).await {
        Ok(claims) => claims,
        Err(message) => return sso_error(Some(&start.0), &message).into_response(),
    };

    // Map claims -> account.
    let oidc = &state.config.oidc;
    let Some(uid) = claims
        .get(&oidc.uid_field)
        .and_then(Value::as_str)
        .filter(|uid| !uid.is_empty())
    else {
        return sso_error(
            Some(&start.0),
            &format!("the ID token has no {:?} claim", oidc.uid_field),
        )
        .into_response();
    };
    let email = claims
        .get(&oidc.email_address_field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();

    let user = match resolve_user(&state, &store, uid, &email, &claims).await {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    let _ = store
        .record_auth_event(NewAuthEvent {
            user_id: Some(user.id),
            email_address: Some(user.email_address.clone()),
            event: "sso.login".into(),
            ip_address: client_ip(&headers),
            user_agent: None,
        })
        .await;

    match issue_session(&store, &state, &user, &headers).await {
        Ok((token, session)) => {
            // With a frontend configured, hand the token over in the URL
            // fragment (fragments never reach server logs).
            if let Some(base) = state.config.auth.frontend_url.as_deref() {
                let url = format!(
                    "{}/auth/callback#session_token={}",
                    base.trim_end_matches('/'),
                    token
                );
                return Redirect::temporary(&url).into_response();
            }
            render_success(
                Some(&start.0),
                StatusCode::CREATED,
                json!({
                    "session_token": token,
                    "expires_at": session.expires_at,
                    "user": user_json(&user),
                }),
            )
            .into_response()
        }
        Err(error) => crate::app::render_store_error(Some(&start.0), error).into_response(),
    }
}

/// Find, link, or provision the account for a validated set of claims.
async fn resolve_user(
    state: &Arc<ApiState>,
    store: &Arc<dyn AuthStore>,
    uid: &str,
    email: &str,
    claims: &serde_json::Map<String, Value>,
) -> Result<camelmailer_core::User, ApiResponse> {
    let map_store_error = |error: StoreError| crate::app::render_store_error(None, error);
    let ensure_enabled = |user_auth: Option<camelmailer_core::UserAuth>| {
        if user_auth.map(|auth| auth.disabled).unwrap_or(false) {
            Err(crate::app::render_error(
                None,
                axum::http::StatusCode::FORBIDDEN,
                "AccountDisabled",
                "This account has been deactivated",
            ))
        } else {
            Ok(())
        }
    };

    // 1. already linked
    if let Some(user) = store.user_by_oidc_sub(uid).await.map_err(map_store_error)? {
        ensure_enabled(store.user_auth(user.id).await.map_err(map_store_error)?)?;
        return Ok(user);
    }

    let oidc = &state.config.oidc;
    if email.is_empty() {
        return Err(sso_error(
            None,
            &format!("the ID token has no {:?} claim", oidc.email_address_field),
        ));
    }
    if !oidc.allowed_email_domains.is_empty() {
        let domain = email.rsplit('@').next().unwrap_or("");
        if !oidc
            .allowed_email_domains
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(domain))
        {
            return Err(sso_error(
                None,
                "this email domain is not allowed to sign in via SSO",
            ));
        }
    }

    // 2. link an existing account by email
    if let Some(user) = store.user_by_email(email).await.map_err(map_store_error)? {
        ensure_enabled(store.user_auth(user.id).await.map_err(map_store_error)?)?;
        store
            .set_oidc_sub(user.id, uid)
            .await
            .map_err(map_store_error)?;
        return Ok(user);
    }

    // 3. provision
    if !oidc.auto_provision {
        return Err(sso_error(
            None,
            "no account exists for this identity and provisioning is disabled",
        ));
    }
    let name = claims
        .get(&oidc.name_field)
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (first_name, last_name) = match name.split_once(' ') {
        Some((first, last)) => (first.to_string(), last.to_string()),
        None => (name.to_string(), String::new()),
    };
    let user = state
        .store
        .create_user(camelmailer_core::NewUser {
            email_address: email.to_string(),
            first_name,
            last_name,
            admin: false,
        })
        .await
        .map_err(map_store_error)?;
    store
        .set_oidc_sub(user.id, uid)
        .await
        .map_err(map_store_error)?;
    let _ = store
        .record_auth_event(NewAuthEvent {
            user_id: Some(user.id),
            email_address: Some(user.email_address.clone()),
            event: "sso.provision".into(),
            ip_address: None,
            user_agent: None,
        })
        .await;
    Ok(user)
}

/// Verify signature (JWKS), `iss`, `aud`, `exp` and `nonce`; returns the
/// claim set.
async fn validate_id_token(
    state: &Arc<ApiState>,
    discovery: &Discovery,
    id_token: &str,
    expected_nonce: &str,
) -> Result<serde_json::Map<String, Value>, String> {
    use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};

    let header = decode_header(id_token).map_err(|error| format!("malformed id_token: {error}"))?;
    let algorithm = match header.alg {
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => header.alg,
        other => return Err(format!("unsupported id_token algorithm {other:?}")),
    };

    let jwks = reqwest::get(&discovery.jwks_uri)
        .await
        .map_err(|error| format!("could not fetch the provider JWKS: {error}"))?
        .json::<Jwks>()
        .await
        .map_err(|error| format!("invalid JWKS document: {error}"))?;
    let jwk = jwks
        .keys
        .iter()
        .filter(|key| key.kty == "RSA" && key.n.is_some() && key.e.is_some())
        .find(|key| match (&header.kid, &key.kid) {
            (Some(kid), Some(key_kid)) => kid == key_kid,
            _ => true,
        })
        .ok_or("no matching RSA key in the provider JWKS")?;
    let decoding_key =
        DecodingKey::from_rsa_components(jwk.n.as_deref().unwrap(), jwk.e.as_deref().unwrap())
            .map_err(|error| format!("invalid JWK: {error}"))?;

    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[&discovery.issuer]);
    validation.set_audience(&[state.config.oidc.identifier.as_deref().unwrap_or("")]);
    let data = decode::<serde_json::Map<String, Value>>(id_token, &decoding_key, &validation)
        .map_err(|error| format!("id_token validation failed: {error}"))?;

    match data.claims.get("nonce").and_then(Value::as_str) {
        Some(nonce) if nonce == expected_nonce => Ok(data.claims),
        _ => Err("id_token nonce mismatch".into()),
    }
}

/// Build the public `/api/v2/auth/oidc` router.
pub fn build_oidc_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .nest(
            "/api/v2/auth/oidc",
            Router::new()
                .route("/start", get(oidc_start))
                .route("/callback", get(oidc_callback))
                .with_state(state),
        )
        .layer(middleware::from_fn(
            |request: Request, next: axum::middleware::Next| timing_middleware(request, next),
        ))
}
