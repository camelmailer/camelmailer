//! Social sign-in (`/api/v2/auth/sso/{id}/*`) — multiple providers such
//! as Google, Microsoft and GitHub side by side, configured as
//! `auth.sso_providers`. Independent of the single enterprise `oidc`
//! group (`/api/v2/auth/oidc/*`), which keeps working unchanged.
//!
//! `type: oidc` providers run the same authorization-code flow with PKCE
//! (S256), server-side state and full ID-token validation as the
//! enterprise OIDC module. Microsoft's multi-tenant `common` issuer is
//! handled specially: when the configured issuer ends in `/common/v2.0`,
//! the `iss` claim varies per tenant and is validated against the
//! pattern `https://login.microsoftonline.com/<tenant-guid>/v2.0`
//! instead of strict equality — signature (Microsoft's common JWKS
//! covers all tenants), `aud`, `exp` and `nonce` stay hard checks.
//!
//! `type: github` providers run GitHub's plain OAuth2 code flow
//! (GitHub does not implement OIDC): the callback exchanges the code,
//! then reads `/user` and `/user/emails` and requires a verified email
//! address (`SSOEmailUnavailable` otherwise). The HTTP calls sit behind
//! the [`GithubOauth`] trait so router tests can run against a local
//! mock GitHub.
//!
//! `GET /api/v2/auth/features` tells the frontend which providers to
//! render (`{ id, name, type }` — never secrets).

use async_trait::async_trait;
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use camelmailer_config::{SsoProvider, SsoProviderType};
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
use crate::oidc::{fetch_discovery, sso_error, urlencode, Discovery};

const STATE_TTL_MINUTES: i64 = 10;

// --------------------------------------------------------------- GitHub

/// The fields of `GET /user` we need.
#[derive(Debug, Clone, Deserialize)]
pub struct GithubUser {
    pub id: u64,
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// One entry of `GET /user/emails`.
#[derive(Debug, Clone, Deserialize)]
pub struct GithubEmail {
    pub email: String,
    #[serde(default)]
    pub primary: bool,
    #[serde(default)]
    pub verified: bool,
}

/// GitHub's OAuth2 + REST surface, behind a trait so router tests can
/// point it at a local mock server (the same idea as the mock IdP in the
/// OIDC tests).
#[async_trait]
pub trait GithubOauth: Send + Sync {
    /// The browser-facing authorization endpoint.
    fn authorize_endpoint(&self) -> String;
    /// Redeem an authorization code for an access token.
    async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
    ) -> Result<String, String>;
    async fn fetch_user(&self, access_token: &str) -> Result<GithubUser, String>;
    async fn fetch_emails(&self, access_token: &str) -> Result<Vec<GithubEmail>, String>;
}

/// The production [`GithubOauth`]: github.com/login/oauth + api.github.com.
pub struct HttpGithub {
    oauth_base: String,
    api_base: String,
}

impl Default for HttpGithub {
    fn default() -> Self {
        Self {
            oauth_base: "https://github.com/login/oauth".into(),
            api_base: "https://api.github.com".into(),
        }
    }
}

impl HttpGithub {
    /// Point both surfaces at other base URLs (tests use a local mock).
    pub fn with_base_urls(oauth_base: &str, api_base: &str) -> Self {
        Self {
            oauth_base: oauth_base.trim_end_matches('/').to_string(),
            api_base: api_base.trim_end_matches('/').to_string(),
        }
    }

    async fn api_get<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        access_token: &str,
    ) -> Result<T, String> {
        let response = reqwest::Client::new()
            .get(format!("{}{path}", self.api_base))
            .header("authorization", format!("Bearer {access_token}"))
            .header("accept", "application/vnd.github+json")
            .header("user-agent", "camelmailer")
            .send()
            .await
            .map_err(|error| format!("could not reach the GitHub API: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "the GitHub API answered HTTP {} for {path}",
                response.status()
            ));
        }
        response
            .json::<T>()
            .await
            .map_err(|error| format!("invalid GitHub API response for {path}: {error}"))
    }
}

#[async_trait]
impl GithubOauth for HttpGithub {
    fn authorize_endpoint(&self) -> String {
        format!("{}/authorize", self.oauth_base)
    }

    async fn exchange_code(
        &self,
        client_id: &str,
        client_secret: &str,
        code: &str,
        redirect_uri: &str,
    ) -> Result<String, String> {
        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: Option<String>,
        }
        let response = reqwest::Client::new()
            .post(format!("{}/access_token", self.oauth_base))
            .header("accept", "application/json")
            .form(&[
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("code", code),
                ("redirect_uri", redirect_uri),
            ])
            .send()
            .await
            .map_err(|error| format!("token exchange failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "token exchange failed with HTTP {}",
                response.status()
            ));
        }
        response
            .json::<TokenResponse>()
            .await
            .map_err(|error| format!("invalid token response: {error}"))?
            .access_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "the token response carried no access_token".into())
    }

    async fn fetch_user(&self, access_token: &str) -> Result<GithubUser, String> {
        self.api_get("/user", access_token).await
    }

    async fn fetch_emails(&self, access_token: &str) -> Result<Vec<GithubEmail>, String> {
        self.api_get("/user/emails", access_token).await
    }
}

// -------------------------------------------------------------- helpers

fn provider_not_found(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::NOT_FOUND,
        "SSOProviderNotFound",
        "No such sign-in provider is configured on this instance",
    )
}

fn email_unavailable(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(
        start,
        StatusCode::UNPROCESSABLE_ENTITY,
        "SSOEmailUnavailable",
        message,
    )
}

fn find_provider<'a>(state: &'a ApiState, id: &str) -> Option<&'a SsoProvider> {
    state
        .config
        .auth
        .sso_providers
        .iter()
        .find(|provider| provider.id == id)
}

/// The redirect URI registered with the provider for this instance.
fn redirect_uri(state: &ApiState, provider_id: &str) -> String {
    format!(
        "{}://{}/api/v2/auth/sso/{}/callback",
        state.config.camelmailer.web_protocol, state.config.camelmailer.web_hostname, provider_id
    )
}

/// The storage key for an in-flight login. Namespaced per provider so a
/// state started with one provider can never be redeemed on another's
/// callback (and never collides with the enterprise `oidc` flow, whose
/// keys are bare tokens).
fn state_key(provider_id: &str, token: &str) -> String {
    format!("sso:{provider_id}:{token}")
}

/// Does the configured issuer designate Microsoft's multi-tenant
/// `common` endpoint? Its `iss` claim varies per tenant.
fn is_microsoft_common_issuer(configured_issuer: &str) -> bool {
    configured_issuer
        .trim_end_matches('/')
        .ends_with("/common/v2.0")
}

/// Strict shape check for a Microsoft per-tenant issuer:
/// `https://login.microsoftonline.com/<tenant-guid>/v2.0`.
fn is_microsoft_tenant_issuer(iss: &str) -> bool {
    iss.strip_prefix("https://login.microsoftonline.com/")
        .and_then(|rest| rest.strip_suffix("/v2.0"))
        .is_some_and(is_guid)
}

fn is_guid(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            _ => byte.is_ascii_hexdigit(),
        })
}

// -------------------------------------------------------------- /start

async fn sso_start(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let Some(provider) = find_provider(&state, &id) else {
        return provider_not_found(Some(&start.0)).into_response();
    };
    let Some(store) = state.auth_store.clone() else {
        return sso_error(Some(&start.0), "accounts require persistent storage").into_response();
    };

    let login_state = auth::generate_auth_token();
    let nonce = auth::generate_auth_token();
    let pkce_verifier = auth::generate_auth_token();
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce_verifier.as_bytes()));

    let authorization_endpoint = match provider.provider_type {
        SsoProviderType::Oidc => match fetch_discovery(&provider.issuer).await {
            Ok(discovery) => discovery.authorization_endpoint,
            Err(message) => return sso_error(Some(&start.0), &message).into_response(),
        },
        SsoProviderType::Github => state.sso_github.authorize_endpoint(),
    };

    if let Err(error) = store
        .create_oidc_state(
            &state_key(&provider.id, &login_state),
            &pkce_verifier,
            &nonce,
            Utc::now() + Duration::minutes(STATE_TTL_MINUTES),
        )
        .await
    {
        return crate::app::render_store_error(Some(&start.0), error).into_response();
    }

    let scope = match provider.provider_type {
        SsoProviderType::Oidc => "openid email profile",
        // `user:email` unlocks /user/emails for the verified address.
        SsoProviderType::Github => "read:user user:email",
    };
    // GitHub ignores PKCE, but sending a challenge is harmless and keeps
    // the two flows symmetrical.
    let url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
        authorization_endpoint,
        urlencode(&provider.client_id),
        urlencode(&redirect_uri(&state, &provider.id)),
        urlencode(scope),
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

/// The identity a provider vouched for, normalized across provider types.
struct SsoIdentity {
    /// The provider's stable subject (OIDC `sub`, GitHub user id).
    subject: String,
    /// Lowercased email address ("" when the provider supplied none).
    email: String,
    name: String,
}

async fn sso_callback(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Path(id): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(provider) = find_provider(&state, &id) else {
        return provider_not_found(Some(&start.0)).into_response();
    };
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

    // Redeem the state — single use, expiring, bound to this provider.
    let consumed = store
        .consume_oidc_state(&state_key(&provider.id, &login_state), Utc::now())
        .await;
    let (pkce_verifier, nonce) = match consumed {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return sso_error(Some(&start.0), "the login state is invalid or has expired")
                .into_response()
        }
        Err(error) => return crate::app::render_store_error(Some(&start.0), error).into_response(),
    };

    let identity = match provider.provider_type {
        SsoProviderType::Oidc => {
            match oidc_identity(&state, provider, &code, &pkce_verifier, &nonce).await {
                Ok(identity) => identity,
                Err(message) => return sso_error(Some(&start.0), &message).into_response(),
            }
        }
        SsoProviderType::Github => match github_identity(&state, provider, &code).await {
            Ok(Some(identity)) => identity,
            Ok(None) => {
                return email_unavailable(
                    Some(&start.0),
                    "the GitHub account has no verified email address",
                )
                .into_response()
            }
            Err(message) => return sso_error(Some(&start.0), &message).into_response(),
        },
    };

    let user = match resolve_user(&state, &store, provider, &identity).await {
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

/// OIDC leg of the callback: code exchange + full ID-token validation.
async fn oidc_identity(
    state: &Arc<ApiState>,
    provider: &SsoProvider,
    code: &str,
    pkce_verifier: &str,
    expected_nonce: &str,
) -> Result<SsoIdentity, String> {
    let discovery = fetch_discovery(&provider.issuer).await?;

    let form: Vec<(&str, String)> = vec![
        ("grant_type", "authorization_code".into()),
        ("code", code.into()),
        ("redirect_uri", redirect_uri(state, &provider.id)),
        ("client_id", provider.client_id.clone()),
        ("client_secret", provider.client_secret.clone()),
        ("code_verifier", pkce_verifier.into()),
    ];
    let response = reqwest::Client::new()
        .post(&discovery.token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|error| format!("token exchange failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "token exchange failed with HTTP {}",
            response.status()
        ));
    }
    let token_response = response
        .json::<TokenResponse>()
        .await
        .map_err(|error| format!("invalid token response: {error}"))?;
    let Some(id_token) = token_response.id_token else {
        return Err("the token response carried no id_token".into());
    };

    let claims = validate_id_token(provider, &discovery, &id_token, expected_nonce).await?;

    let Some(subject) = claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|sub| !sub.is_empty())
    else {
        return Err("the ID token has no \"sub\" claim".into());
    };
    let email = claims
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let name = claims
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(SsoIdentity {
        subject: subject.to_string(),
        email,
        name,
    })
}

/// Verify signature (JWKS), `iss`, `aud`, `exp` and `nonce`. For a
/// configured Microsoft `common` issuer the `iss` claim is validated
/// against the per-tenant pattern instead of strict equality; everything
/// else stays hard.
async fn validate_id_token(
    provider: &SsoProvider,
    discovery: &Discovery,
    id_token: &str,
    expected_nonce: &str,
) -> Result<serde_json::Map<String, Value>, String> {
    use jsonwebtoken::{decode, decode_header, Algorithm, Validation};

    let header = decode_header(id_token).map_err(|error| format!("malformed id_token: {error}"))?;
    let algorithm = match header.alg {
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => header.alg,
        other => return Err(format!("unsupported id_token algorithm {other:?}")),
    };
    // The configured issuer's JWKS — for Microsoft `common` that document
    // covers the keys of every tenant.
    let decoding_key = crate::oidc::decoding_key_from_jwks(&discovery.jwks_uri, &header).await?;

    let multi_tenant = is_microsoft_common_issuer(&provider.issuer);
    let mut validation = Validation::new(algorithm);
    validation.set_audience(&[provider.client_id.as_str()]);
    if !multi_tenant {
        validation.set_issuer(&[&discovery.issuer]);
    }
    let data = decode::<serde_json::Map<String, Value>>(id_token, &decoding_key, &validation)
        .map_err(|error| format!("id_token validation failed: {error}"))?;

    if multi_tenant {
        // `iss` is `https://login.microsoftonline.com/{tenantid}/v2.0` —
        // per tenant, so match the shape instead of the configured string.
        match data.claims.get("iss").and_then(Value::as_str) {
            Some(iss) if is_microsoft_tenant_issuer(iss) => {}
            other => {
                return Err(format!(
                    "id_token validation failed: issuer {other:?} is not a Microsoft tenant issuer"
                ))
            }
        }
    }

    match data.claims.get("nonce").and_then(Value::as_str) {
        Some(nonce) if nonce == expected_nonce => Ok(data.claims),
        _ => Err("id_token nonce mismatch".into()),
    }
}

/// GitHub leg of the callback: code exchange, then `/user` +
/// `/user/emails`. `Ok(None)` means "no verified email address".
async fn github_identity(
    state: &Arc<ApiState>,
    provider: &SsoProvider,
    code: &str,
) -> Result<Option<SsoIdentity>, String> {
    let github = &state.sso_github;
    let access_token = github
        .exchange_code(
            &provider.client_id,
            &provider.client_secret,
            code,
            &redirect_uri(state, &provider.id),
        )
        .await?;
    let user = github.fetch_user(&access_token).await?;
    let emails = github.fetch_emails(&access_token).await?;

    // The primary verified address, or failing that any verified one.
    let email = emails
        .iter()
        .find(|email| email.primary && email.verified)
        .or_else(|| emails.iter().find(|email| email.verified));
    let Some(email) = email else {
        return Ok(None);
    };

    let name = user
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&user.login)
        .to_string();
    Ok(Some(SsoIdentity {
        subject: user.id.to_string(),
        email: email.email.to_lowercase(),
        name,
    }))
}

/// Find, link, or provision the account for a verified identity —
/// the same resolution order as the enterprise OIDC flow.
async fn resolve_user(
    state: &Arc<ApiState>,
    store: &Arc<dyn AuthStore>,
    provider: &SsoProvider,
    identity: &SsoIdentity,
) -> Result<camelmailer_core::User, ApiResponse> {
    let map_store_error = |error: StoreError| crate::app::render_store_error(None, error);

    // 1. already linked
    if let Some(user) = store
        .user_by_sso_identity(&provider.id, &identity.subject)
        .await
        .map_err(map_store_error)?
    {
        return Ok(user);
    }

    if identity.email.is_empty() {
        return Err(email_unavailable(
            None,
            "the identity provider supplied no email address",
        ));
    }
    if !provider.allowed_email_domains.is_empty() {
        let domain = identity.email.rsplit('@').next().unwrap_or("");
        if !provider
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
    if let Some(user) = store
        .user_by_email(&identity.email)
        .await
        .map_err(map_store_error)?
    {
        store
            .link_sso_identity(user.id, &provider.id, &identity.subject)
            .await
            .map_err(map_store_error)?;
        return Ok(user);
    }

    // 3. provision
    if !provider.auto_provision {
        return Err(sso_error(
            None,
            "no account exists for this identity and provisioning is disabled",
        ));
    }
    let (first_name, last_name) = match identity.name.split_once(' ') {
        Some((first, last)) => (first.to_string(), last.to_string()),
        None => (identity.name.clone(), String::new()),
    };
    let user = state
        .store
        .create_user(camelmailer_core::NewUser {
            email_address: identity.email.clone(),
            first_name,
            last_name,
            admin: false,
        })
        .await
        .map_err(map_store_error)?;
    store
        .link_sso_identity(user.id, &provider.id, &identity.subject)
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

/// Build the public social-SSO router (`/api/v2/auth/sso/{id}/*` plus the
/// `/api/v2/auth/features` discovery endpoint).
pub fn build_sso_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .nest(
            "/api/v2/auth",
            Router::new()
                .route("/sso/{id}/start", get(sso_start))
                .route("/sso/{id}/callback", get(sso_callback))
                .with_state(state),
        )
        .layer(middleware::from_fn(
            |request: Request, next: axum::middleware::Next| timing_middleware(request, next),
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microsoft_common_issuer_detection() {
        assert!(is_microsoft_common_issuer(
            "https://login.microsoftonline.com/common/v2.0"
        ));
        assert!(is_microsoft_common_issuer(
            "https://login.microsoftonline.com/common/v2.0/"
        ));
        assert!(!is_microsoft_common_issuer(
            "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"
        ));
        assert!(!is_microsoft_common_issuer("https://accounts.google.com"));
    }

    #[test]
    fn microsoft_tenant_issuer_shape() {
        assert!(is_microsoft_tenant_issuer(
            "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"
        ));
        // the `common` placeholder itself is not a tenant issuer
        assert!(!is_microsoft_tenant_issuer(
            "https://login.microsoftonline.com/common/v2.0"
        ));
        // wrong host — the pattern pins Microsoft's login host
        assert!(!is_microsoft_tenant_issuer(
            "https://evil.example/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"
        ));
        // http, missing version suffix, malformed guid
        assert!(!is_microsoft_tenant_issuer(
            "http://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0"
        ));
        assert!(!is_microsoft_tenant_issuer(
            "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad"
        ));
        assert!(!is_microsoft_tenant_issuer(
            "https://login.microsoftonline.com/not-a-guid/v2.0"
        ));
        assert!(!is_microsoft_tenant_issuer(
            "https://login.microsoftonline.com/9188040d6c674c5bb11236a304b66dad/v2.0"
        ));
    }
}
