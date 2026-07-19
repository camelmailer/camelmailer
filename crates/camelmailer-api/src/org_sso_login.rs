//! Tenant SSO login flow (`/api/v2/auth/org-sso/…`). A signed-out user's
//! email domain resolves to the organization that verified it; that org's
//! enabled connections drive the sign-in. This supplements the
//! instance-wide `/api/v2/auth/{oidc,saml,sso}` flows and never touches
//! them.
//!
//! Accounts are linked per connection via the `sso_identities` table
//! (provider key `org-sso-{connection_id}`), so an org connection never
//! collides with the single enterprise `oidc_sub`. Provisioned users join
//! the connection's organization with its configured default role, and a
//! login is only accepted when the identity provider's email belongs to a
//! domain the organization has verified.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{middleware, Form, Json, Router};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use camelmailer_core::{
    auth, AuthStore, Id, NewAuthEvent, NewUser, OrgSsoConnection, OrgSsoStore, Role, SsoKind,
    StoreError, User,
};
use chrono::{Duration, Utc};
use flate2::write::DeflateEncoder;
use flate2::Compression;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::sync::Arc;

use crate::app::{
    render_error, render_store_error, render_success, timing_middleware, ApiResponse, ApiState,
    RequestStart,
};
use crate::auth_api::{client_ip, issue_session, user_json};
use crate::oidc::{
    decoding_key_from_jwks, fetch_discovery, provisioned_name, sso_error, urlencode, Discovery,
};

const STATE_TTL_MINUTES: i64 = 10;

fn config_str(config: &Value, key: &str) -> String {
    config
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The OIDC issuer for a connection, or `None` for kinds that do not use
/// OIDC discovery (GitHub OAuth2 and SAML are handled by their own flows).
fn issuer_for(kind: SsoKind, config: &Value) -> Option<String> {
    let configured = |key: &str| {
        config
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    match kind {
        SsoKind::Oidc => configured("issuer"),
        SsoKind::Google => Some("https://accounts.google.com".to_string()),
        SsoKind::Microsoft => Some(
            configured("issuer")
                .unwrap_or_else(|| "https://login.microsoftonline.com/common/v2.0".to_string()),
        ),
        SsoKind::Github | SsoKind::Saml => None,
    }
}

fn org_redirect_uri(state: &ApiState) -> String {
    format!(
        "{}://{}/api/v2/auth/org-sso/callback",
        state.config.camelmailer.web_protocol, state.config.camelmailer.web_hostname
    )
}

// ------------------------------------------------------------- discover

#[derive(Debug, Deserialize)]
pub(crate) struct DiscoverBody {
    email: String,
}

/// `POST /api/v2/auth/org-sso/discover` — which SSO connections, if any,
/// apply to an email address. Unauthenticated. Returns an empty list when
/// the domain is not verified by any organization, so the login page can
/// simply fall back to password sign-in.
pub(crate) async fn sso_discover(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Json(body): Json<DiscoverBody>,
) -> ApiResponse {
    let Some(store) = state.org_sso_store.clone() else {
        return render_success(Some(&start), StatusCode::OK, json!({ "connections": [] }));
    };
    let domain = body
        .email
        .rsplit('@')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let connections = match store.organization_for_verified_email_domain(&domain).await {
        Ok(Some(org_id)) => store
            .list_org_sso_connections(org_id)
            .await
            .unwrap_or_default(),
        Ok(None) => Vec::new(),
        Err(error) => return render_store_error(Some(&start), error),
    };
    let list: Vec<Value> = connections
        .iter()
        .filter(|connection| connection.enabled)
        .map(|connection| {
            json!({
                "id": connection.id,
                "kind": connection.kind.as_str(),
                "name": connection.name,
                "start_url": format!("/api/v2/auth/org-sso/{}/start", connection.id),
            })
        })
        .collect();
    render_success(Some(&start), StatusCode::OK, json!({ "connections": list }))
}

// --------------------------------------------------------------- /start

async fn load_enabled_connection(
    store: &Arc<dyn OrgSsoStore>,
    start: &RequestStart,
    connection_id: Id,
) -> Result<OrgSsoConnection, Response> {
    match store.org_sso_connection(connection_id).await {
        Ok(Some(connection)) if connection.enabled => Ok(connection),
        Ok(_) => Err(sso_error(
            Some(start),
            "this single sign-on connection is not available",
        )
        .into_response()),
        Err(error) => Err(render_store_error(Some(start), error).into_response()),
    }
}

/// The two account-storage facets a tenant login needs.
type LoginStores = (Arc<dyn AuthStore>, Arc<dyn OrgSsoStore>);

fn stores(state: &ApiState, start: &RequestStart) -> Result<LoginStores, Box<Response>> {
    match (state.auth_store.clone(), state.org_sso_store.clone()) {
        (Some(auth), Some(sso)) => Ok((auth, sso)),
        _ => Err(Box::new(
            sso_error(Some(start), "accounts require persistent storage").into_response(),
        )),
    }
}

pub(crate) async fn org_start(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: HeaderMap,
    Path(connection_id): Path<Id>,
) -> Response {
    let (auth_store, sso_store) = match stores(&state, &start) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };
    let connection = match load_enabled_connection(&sso_store, &start, connection_id).await {
        Ok(connection) => connection,
        Err(response) => return response,
    };
    if connection.kind == SsoKind::Saml {
        return saml_org_start(&state, &auth_store, &start, &headers, &connection).await;
    }
    // The connection id travels in the opaque state so the single callback
    // knows which connection redeemed it. GitHub does not use PKCE or a
    // nonce; the stored values still guard against CSRF and replay.
    let login_state = format!("{connection_id}~{}", auth::generate_auth_token());
    let nonce = auth::generate_auth_token();
    let pkce_verifier = auth::generate_auth_token();
    if let Err(error) = auth_store
        .create_oidc_state(
            &login_state,
            &pkce_verifier,
            &nonce,
            Utc::now() + Duration::minutes(STATE_TTL_MINUTES),
        )
        .await
    {
        return render_store_error(Some(&start), error).into_response();
    }

    let client_id = config_str(&connection.config, "client_id");
    let redirect = org_redirect_uri(&state);
    let url = match connection.kind {
        SsoKind::Github => format!(
            "{}?client_id={}&redirect_uri={}&scope={}&state={}",
            state.sso_github.authorize_endpoint(),
            urlencode(&client_id),
            urlencode(&redirect),
            urlencode("read:user user:email"),
            login_state,
        ),
        _ => {
            let Some(issuer) = issuer_for(connection.kind, &connection.config) else {
                return sso_error(
                    Some(&start),
                    "this connection type cannot be used for sign-in yet",
                )
                .into_response();
            };
            let discovery = match fetch_discovery(&issuer).await {
                Ok(discovery) => discovery,
                Err(message) => return sso_error(Some(&start), &message).into_response(),
            };
            let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce_verifier.as_bytes()));
            format!(
                "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}&code_challenge={}&code_challenge_method=S256",
                discovery.authorization_endpoint,
                urlencode(&client_id),
                urlencode(&redirect),
                urlencode("openid email profile"),
                login_state,
                nonce,
                challenge,
            )
        }
    };

    let wants_json = headers
        .get("accept")
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("application/json"))
        .unwrap_or(false);
    if wants_json {
        render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "authorization_url": url }),
        )
        .into_response()
    } else {
        Redirect::temporary(&url).into_response()
    }
}

// ------------------------------------------------------------ /callback

#[derive(Debug, Deserialize)]
pub(crate) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

pub(crate) async fn org_callback(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: HeaderMap,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let (auth_store, sso_store) = match stores(&state, &start) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };
    if let Some(error) = query.error {
        let description = query.error_description.unwrap_or_default();
        return sso_error(
            Some(&start),
            &format!("the identity provider reported: {error} {description}"),
        )
        .into_response();
    }
    let (Some(code), Some(login_state)) = (query.code, query.state) else {
        return sso_error(Some(&start), "missing code or state parameter").into_response();
    };
    let Some(connection_id) = login_state
        .split('~')
        .next()
        .and_then(|prefix| prefix.parse::<Id>().ok())
    else {
        return sso_error(Some(&start), "malformed login state").into_response();
    };

    let (pkce_verifier, nonce) = match auth_store
        .consume_oidc_state(&login_state, Utc::now())
        .await
    {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return sso_error(Some(&start), "the login state is invalid or has expired")
                .into_response()
        }
        Err(error) => return render_store_error(Some(&start), error).into_response(),
    };
    let connection = match load_enabled_connection(&sso_store, &start, connection_id).await {
        Ok(connection) => connection,
        Err(response) => return response,
    };
    // Turn the provider's response into (uid, email, name), by protocol.
    let (uid, email, name): (String, String, String) = match connection.kind {
        SsoKind::Github => match github_identity(&state, &connection, code).await {
            Ok(identity) => identity,
            Err(response) => return *response,
        },
        _ => {
            let Some(issuer) = issuer_for(connection.kind, &connection.config) else {
                return sso_error(
                    Some(&start),
                    "this connection type cannot be used for sign-in yet",
                )
                .into_response();
            };
            let client_id = config_str(&connection.config, "client_id");
            let client_secret = config_str(&connection.config, "client_secret");
            let discovery = match fetch_discovery(&issuer).await {
                Ok(discovery) => discovery,
                Err(message) => return sso_error(Some(&start), &message).into_response(),
            };
            let mut form: Vec<(&str, String)> = vec![
                ("grant_type", "authorization_code".into()),
                ("code", code),
                ("redirect_uri", org_redirect_uri(&state)),
                ("client_id", client_id.clone()),
                ("code_verifier", pkce_verifier),
            ];
            if !client_secret.is_empty() {
                form.push(("client_secret", client_secret));
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
                            return sso_error(
                                Some(&start),
                                &format!("invalid token response: {error}"),
                            )
                            .into_response()
                        }
                    }
                }
                Ok(response) => {
                    return sso_error(
                        Some(&start),
                        &format!("token exchange failed with HTTP {}", response.status()),
                    )
                    .into_response()
                }
                Err(error) => {
                    return sso_error(Some(&start), &format!("token exchange failed: {error}"))
                        .into_response()
                }
            };
            let Some(id_token) = token_response.id_token else {
                return sso_error(Some(&start), "the token response carried no id_token")
                    .into_response();
            };
            let claims = match validate_id_token(&discovery, &id_token, &nonce, &client_id).await {
                Ok(claims) => claims,
                Err(message) => return sso_error(Some(&start), &message).into_response(),
            };
            let Some(uid) = claims
                .get("sub")
                .and_then(Value::as_str)
                .filter(|uid| !uid.is_empty())
            else {
                return sso_error(Some(&start), "the id_token has no sub claim").into_response();
            };
            let email = claims
                .get("email")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_lowercase();
            // Prefer the combined `name`; fall back to discrete
            // `given_name`/`family_name` so an IdP that omits `name` (or
            // sends only one part) still yields a usable name downstream.
            let claim = |key: &str| {
                claims
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            };
            let name = match claim("name") {
                combined if !combined.is_empty() => combined,
                _ => format!("{} {}", claim("given_name"), claim("family_name"))
                    .trim()
                    .to_string(),
            };
            (uid.to_string(), email, name)
        }
    };

    let user = match resolve_org_user(
        &state,
        &auth_store,
        &sso_store,
        &connection,
        &uid,
        &email,
        &name,
    )
    .await
    {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    complete_login(&state, &auth_store, &start, &headers, &user).await
}

/// Record the audit event and issue the session — the shared tail of every
/// tenant login (OIDC family, GitHub, SAML).
async fn complete_login(
    state: &Arc<ApiState>,
    auth_store: &Arc<dyn AuthStore>,
    start: &RequestStart,
    headers: &HeaderMap,
    user: &User,
) -> Response {
    let _ = auth_store
        .record_auth_event(NewAuthEvent {
            user_id: Some(user.id),
            email_address: Some(user.email_address.clone()),
            event: "sso.login".into(),
            ip_address: client_ip(headers),
            user_agent: None,
        })
        .await;

    match issue_session(auth_store, state, user, headers).await {
        Ok((token, session)) => {
            if let Some(base) = state.config.auth.frontend_url.as_deref() {
                let url = format!(
                    "{}/auth/callback#session_token={}",
                    base.trim_end_matches('/'),
                    token
                );
                return Redirect::temporary(&url).into_response();
            }
            render_success(
                Some(start),
                StatusCode::CREATED,
                json!({
                    "session_token": token,
                    "expires_at": session.expires_at,
                    "user": user_json(user),
                }),
            )
            .into_response()
        }
        Err(error) => render_store_error(Some(start), error).into_response(),
    }
}

/// Redeem a GitHub authorization code and return `(uid, email, name)`.
/// GitHub is OAuth2 without an id_token, so the account is identified by
/// its numeric user id and the primary verified email.
async fn github_identity(
    state: &Arc<ApiState>,
    connection: &OrgSsoConnection,
    code: String,
) -> Result<(String, String, String), Box<Response>> {
    let fail = |message: String| Box::new(sso_error(None, &message).into_response());
    let github = &state.sso_github;
    let client_id = config_str(&connection.config, "client_id");
    let client_secret = config_str(&connection.config, "client_secret");
    let access_token = github
        .exchange_code(&client_id, &client_secret, &code, &org_redirect_uri(state))
        .await
        .map_err(fail)?;
    let user = github.fetch_user(&access_token).await.map_err(fail)?;
    let emails = github.fetch_emails(&access_token).await.map_err(fail)?;

    // The primary verified address, or any verified one.
    let Some(email) = emails
        .iter()
        .find(|email| email.primary && email.verified)
        .or_else(|| emails.iter().find(|email| email.verified))
    else {
        return Err(Box::new(
            sso_error(None, "the GitHub account has no verified email address").into_response(),
        ));
    };
    let name = user
        .name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(&user.login)
        .to_string();
    Ok((user.id.to_string(), email.email.to_lowercase(), name))
}

/// Verify the id_token signature (JWKS), `iss`, `aud` and `nonce`.
async fn validate_id_token(
    discovery: &Discovery,
    id_token: &str,
    expected_nonce: &str,
    audience: &str,
) -> Result<Map<String, Value>, String> {
    use jsonwebtoken::{decode, decode_header, Algorithm, Validation};

    let header = decode_header(id_token).map_err(|error| format!("malformed id_token: {error}"))?;
    let algorithm = match header.alg {
        Algorithm::RS256 | Algorithm::RS384 | Algorithm::RS512 => header.alg,
        other => return Err(format!("unsupported id_token algorithm {other:?}")),
    };
    let decoding_key = decoding_key_from_jwks(&discovery.jwks_uri, &header).await?;

    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[&discovery.issuer]);
    validation.set_audience(&[audience]);
    let data = decode::<Map<String, Value>>(id_token, &decoding_key, &validation)
        .map_err(|error| format!("id_token validation failed: {error}"))?;

    match data.claims.get("nonce").and_then(Value::as_str) {
        Some(nonce) if nonce == expected_nonce => Ok(data.claims),
        _ => Err("id_token nonce mismatch".into()),
    }
}

/// Find, link, or provision the account for a validated tenant SSO login,
/// and make sure it belongs to the connection's organization.
async fn resolve_org_user(
    state: &Arc<ApiState>,
    auth: &Arc<dyn AuthStore>,
    sso: &Arc<dyn OrgSsoStore>,
    connection: &OrgSsoConnection,
    uid: &str,
    email: &str,
    name: &str,
) -> Result<User, ApiResponse> {
    let map_store_error = |error: StoreError| render_store_error(None, error);
    let provider = format!("org-sso-{}", connection.id);
    let org_id = connection.organization_id;

    // The identity provider's email must belong to a domain this exact
    // organization has verified. Without this a misconfigured IdP could
    // provision arbitrary accounts into the tenant.
    if email.is_empty() {
        return Err(sso_error(
            None,
            "the identity provider returned no email address",
        ));
    }
    let domain = email.rsplit('@').next().unwrap_or_default();
    match sso
        .organization_for_verified_email_domain(domain)
        .await
        .map_err(map_store_error)?
    {
        Some(owner) if owner == org_id => {}
        _ => {
            return Err(sso_error(
                None,
                "this email domain is not verified for the organization",
            ))
        }
    }

    let ensure_enabled = |user_auth: Option<camelmailer_core::UserAuth>| {
        if user_auth.map(|auth| auth.disabled).unwrap_or(false) {
            Err(render_error(
                None,
                StatusCode::FORBIDDEN,
                "AccountDisabled",
                "This account has been deactivated",
            ))
        } else {
            Ok(())
        }
    };

    // 1. already linked to this connection
    if let Some(user) = auth
        .user_by_sso_identity(&provider, uid)
        .await
        .map_err(map_store_error)?
    {
        ensure_enabled(auth.user_auth(user.id).await.map_err(map_store_error)?)?;
        ensure_membership(auth, org_id, user.id, connection.default_role).await?;
        return Ok(user);
    }

    // 2. an existing account with this email
    if let Some(user) = auth.user_by_email(email).await.map_err(map_store_error)? {
        ensure_enabled(auth.user_auth(user.id).await.map_err(map_store_error)?)?;
        auth.link_sso_identity(user.id, &provider, uid)
            .await
            .map_err(map_store_error)?;
        ensure_membership(auth, org_id, user.id, connection.default_role).await?;
        return Ok(user);
    }

    // 3. provision a fresh account and join the organization
    if !connection.auto_provision {
        return Err(sso_error(
            None,
            "no account exists for this identity and provisioning is disabled for this connection",
        ));
    }
    // `given_name`/`family_name` were already folded into `name` upstream;
    // the helper still guarantees a non-empty first name via the email.
    let (first_name, last_name) = provisioned_name(name, "", "", email);
    let user = state
        .store
        .create_user(NewUser {
            email_address: email.to_string(),
            first_name,
            last_name,
            admin: false,
        })
        .await
        .map_err(map_store_error)?;
    auth.link_sso_identity(user.id, &provider, uid)
        .await
        .map_err(map_store_error)?;
    auth.upsert_membership(org_id, user.id, connection.default_role)
        .await
        .map_err(map_store_error)?;
    let _ = auth
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

/// Add the user to the organization if they are not already a member.
/// Never downgrades an existing higher role.
async fn ensure_membership(
    auth: &Arc<dyn AuthStore>,
    org_id: Id,
    user_id: Id,
    role: Role,
) -> Result<(), ApiResponse> {
    let map_store_error = |error: StoreError| render_store_error(None, error);
    if auth
        .membership(org_id, user_id)
        .await
        .map_err(map_store_error)?
        .is_none()
    {
        auth.upsert_membership(org_id, user_id, role)
            .await
            .map_err(map_store_error)?;
    }
    Ok(())
}

// ----------------------------------------------------------------- SAML

fn org_acs_url(state: &ApiState) -> String {
    format!(
        "{}://{}/api/v2/auth/org-sso/acs",
        state.config.camelmailer.web_protocol, state.config.camelmailer.web_hostname
    )
}

/// The SP entity id for a connection: its configured `sp_entity_id`, or
/// the installation origin (matching the instance-wide default).
fn sp_entity_for(state: &ApiState, connection: &OrgSsoConnection) -> String {
    let configured = config_str(&connection.config, "sp_entity_id");
    if configured.is_empty() {
        format!(
            "{}://{}",
            state.config.camelmailer.web_protocol, state.config.camelmailer.web_hostname
        )
    } else {
        configured
    }
}

/// Begin a SAML login for a tenant connection: an AuthnRequest over the
/// HTTP-Redirect binding, aimed at the connection's IdP. The request id
/// carries a `_c{connection_id}.` prefix, so the signed `InResponseTo`
/// later binds the response to this exact connection; RelayState carries
/// the id for routing before validation.
async fn saml_org_start(
    state: &Arc<ApiState>,
    auth_store: &Arc<dyn AuthStore>,
    start: &RequestStart,
    headers: &HeaderMap,
    connection: &OrgSsoConnection,
) -> Response {
    use crate::saml::{xml_escape, NS_ASSERTION, NS_PROTOCOL};

    let idp_sso_url = config_str(&connection.config, "idp_sso_url");
    if idp_sso_url.is_empty() {
        return sso_error(Some(start), "this connection has no idp_sso_url").into_response();
    }

    let request_id = format!("_c{}.{}", connection.id, auth::generate_auth_token());
    if let Err(error) = auth_store
        .create_saml_request(
            &request_id,
            Utc::now() + Duration::minutes(STATE_TTL_MINUTES),
        )
        .await
    {
        return render_store_error(Some(start), error).into_response();
    }

    let authn_request = format!(
        "<samlp:AuthnRequest xmlns:samlp=\"{NS_PROTOCOL}\" xmlns:saml=\"{NS_ASSERTION}\" \
         ID=\"{id}\" Version=\"2.0\" IssueInstant=\"{instant}\" Destination=\"{sso}\" \
         AssertionConsumerServiceURL=\"{acs}\" \
         ProtocolBinding=\"urn:oasis:names:tc:SAML:2.0:bindings:HTTP-POST\">\
         <saml:Issuer>{issuer}</saml:Issuer>\
         </samlp:AuthnRequest>",
        id = request_id,
        instant = Utc::now().format("%Y-%m-%dT%H:%M:%SZ"),
        sso = xml_escape(&idp_sso_url),
        acs = xml_escape(&org_acs_url(state)),
        issuer = xml_escape(&sp_entity_for(state, connection)),
    );

    // HTTP-Redirect binding: raw deflate, then base64, then URL-encode.
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    let deflated = encoder
        .write_all(authn_request.as_bytes())
        .and_then(|()| encoder.finish());
    let deflated = match deflated {
        Ok(bytes) => bytes,
        Err(error) => {
            return sso_error(
                Some(start),
                &format!("could not encode the request: {error}"),
            )
            .into_response()
        }
    };
    let separator = if idp_sso_url.contains('?') { '&' } else { '?' };
    let url = format!(
        "{idp_sso_url}{separator}SAMLRequest={}&RelayState={}",
        urlencode(&STANDARD.encode(deflated)),
        connection.id,
    );

    let wants_json = headers
        .get("accept")
        .and_then(|value| value.to_str().ok())
        .map(|accept| accept.contains("application/json"))
        .unwrap_or(false);
    if wants_json {
        render_success(
            Some(start),
            StatusCode::OK,
            json!({ "authorization_url": url, "name": connection.name }),
        )
        .into_response()
    } else {
        Redirect::temporary(&url).into_response()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct OrgAcsForm {
    #[serde(rename = "SAMLResponse")]
    saml_response: Option<String>,
    #[serde(rename = "RelayState")]
    relay_state: Option<String>,
}

/// `POST /api/v2/auth/org-sso/acs` — the tenant assertion consumer
/// service. RelayState routes to the connection whose certificate then
/// validates the response; the signed `InResponseTo` must carry the same
/// connection's request prefix, so a response can never be replayed
/// against a different tenant's connection.
pub(crate) async fn org_acs(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: HeaderMap,
    Form(form): Form<OrgAcsForm>,
) -> Response {
    use crate::saml::{attribute_value, extract_email, validate_response};

    let (auth_store, sso_store) = match stores(&state, &start) {
        Ok(pair) => pair,
        Err(response) => return *response,
    };
    let Some(connection_id) = form
        .relay_state
        .as_deref()
        .and_then(|value| value.parse::<Id>().ok())
    else {
        return sso_error(Some(&start), "missing or malformed RelayState").into_response();
    };
    let connection = match load_enabled_connection(&sso_store, &start, connection_id).await {
        Ok(connection) if connection.kind == SsoKind::Saml => connection,
        Ok(_) => {
            return sso_error(Some(&start), "this connection does not speak SAML").into_response()
        }
        Err(response) => return response,
    };

    let Some(encoded) = form.saml_response.filter(|value| !value.is_empty()) else {
        return sso_error(Some(&start), "missing SAMLResponse parameter").into_response();
    };
    let xml = match crate::xmldsig::decode_base64(&encoded).map(String::from_utf8) {
        Ok(Ok(xml)) => xml,
        _ => {
            return sso_error(Some(&start), "SAMLResponse is not valid base64 XML").into_response()
        }
    };

    // The connection's own IdP certificate (inline PEM, or a path for
    // parity with the instance-wide config).
    let configured = config_str(&connection.config, "idp_certificate");
    let pem = if configured.contains("-----BEGIN") {
        configured
    } else if configured.is_empty() {
        return sso_error(Some(&start), "this connection has no idp_certificate").into_response();
    } else {
        match std::fs::read_to_string(&configured) {
            Ok(pem) => pem,
            Err(error) => {
                return sso_error(
                    Some(&start),
                    &format!("could not read idp_certificate {configured:?}: {error}"),
                )
                .into_response()
            }
        }
    };
    let public_key = match crate::xmldsig::public_key_from_certificate_pem(&pem) {
        Ok(key) => key,
        Err(message) => return sso_error(Some(&start), &message).into_response(),
    };

    let now = Utc::now();
    let validated = match validate_response(
        &xml,
        &public_key,
        &sp_entity_for(&state, &connection),
        &org_acs_url(&state),
        now,
    ) {
        Ok(validated) => validated,
        Err(message) => {
            tracing::warn!(%message, connection = connection.id, "rejected tenant SAML response");
            return sso_error(Some(&start), &message).into_response();
        }
    };

    // The signed InResponseTo must name a request this connection issued …
    if !validated
        .in_response_to
        .starts_with(&format!("_c{}.", connection.id))
    {
        return sso_error(
            Some(&start),
            "the response does not answer a login request of this connection",
        )
        .into_response();
    }
    // … that is still outstanding, and each assertion is single use.
    match auth_store
        .consume_saml_request(&validated.in_response_to, now)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return sso_error(
                Some(&start),
                "the login request is unknown, already used, or has expired",
            )
            .into_response()
        }
        Err(error) => return render_store_error(Some(&start), error).into_response(),
    }
    match auth_store
        .register_saml_assertion(&validated.assertion_id, validated.not_on_or_after, now)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return sso_error(Some(&start), "this assertion has already been used").into_response()
        }
        Err(error) => return render_store_error(Some(&start), error).into_response(),
    }

    let Some(email) = extract_email(&validated) else {
        return sso_error(
            Some(&start),
            "the assertion carries no email address (NameID or email attribute)",
        )
        .into_response();
    };
    // The stable subject: the NameID when present, the email otherwise.
    let uid = validated
        .name_id
        .clone()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| email.clone());
    let name = attribute_value(&validated.attributes, &["displayname", "cn", "name"])
        .unwrap_or_else(|| {
            let given = attribute_value(&validated.attributes, &["givenname"]).unwrap_or_default();
            let surname =
                attribute_value(&validated.attributes, &["sn", "surname"]).unwrap_or_default();
            format!("{given} {surname}").trim().to_string()
        });

    let user = match resolve_org_user(
        &state,
        &auth_store,
        &sso_store,
        &connection,
        &uid,
        &email,
        &name,
    )
    .await
    {
        Ok(user) => user,
        Err(response) => return response.into_response(),
    };

    complete_login(&state, &auth_store, &start, &headers, &user).await
}

/// Build the public `/api/v2/auth/org-sso` router.
pub fn build_org_sso_login_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .nest(
            "/api/v2/auth/org-sso",
            Router::new()
                .route("/discover", post(sso_discover))
                .route("/{connection_id}/start", get(org_start))
                .route("/callback", get(org_callback))
                .route("/acs", post(org_acs))
                .with_state(state),
        )
        .layer(middleware::from_fn(
            |request: axum::extract::Request, next: axum::middleware::Next| {
                timing_middleware(request, next)
            },
        ))
}
