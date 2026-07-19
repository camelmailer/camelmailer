//! `/api/v2/auth` — user accounts: password + TOTP login, sessions
//! (Bearer tokens), password change/reset, 2FA enrollment, and (when
//! `auth.allow_registration` is on) self-registration.
//!
//! Responses use the same `{ status, time, data | error }` envelope as the
//! admin API. Error codes a frontend can branch on:
//! `InvalidCredentials`, `AccountLocked`, `TOTPRequired`,
//! `InvalidTOTPCode`, `InvalidToken`, `Unauthorized`,
//! `RegistrationDisabled`, `WebAuthnDisabled`.
//!
//! WebAuthn / passkey endpoints live in [`crate::webauthn`] and are
//! mounted into this router; `GET /features` lets frontends discover
//! which optional login features (passkeys, self-registration, OIDC)
//! this instance exposes.
//! `InvalidCredentials`, `AccountLocked`, `AccountDisabled`,
//! `TOTPRequired`, `InvalidTOTPCode`, `InvalidToken`, `Unauthorized`,
//! `RegistrationDisabled`.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use camelmailer_core::auth::{self, NewAuthEvent, NewAuthSession};
use camelmailer_core::{AuthSession, AuthStore, StoreError, User};
use chrono::{Duration, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::OnceLock;

use crate::app::{
    render_error, render_parameter_missing, render_store_error, render_success,
    render_validation_error, timing_middleware, ApiResponse, ApiState, RequestStart,
};

/// The authenticated principal for the current request, injected by
/// [`session_middleware`].
#[derive(Clone)]
pub(crate) struct CurrentUser {
    pub(crate) user: User,
    pub(crate) token_hash: String,
}

/// A stable Argon2 digest used to equalize the timing of logins against
/// unknown email addresses (user enumeration hardening).
fn dummy_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| auth::hash_password("timing-equalization-dummy").unwrap())
}

fn auth_store(state: &ApiState) -> Option<&Arc<dyn AuthStore>> {
    state.auth_store.as_ref()
}

fn unavailable(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::SERVICE_UNAVAILABLE,
        "AccountsUnavailable",
        "User accounts require persistent storage and are not enabled on this instance",
    )
}

/// The auth store, or the standard `503 AccountsUnavailable` response.
pub(crate) fn auth_store_or_unavailable<'a>(
    state: &'a ApiState,
    start: Option<&RequestStart>,
) -> Result<&'a Arc<dyn AuthStore>, ApiResponse> {
    auth_store(state).ok_or_else(|| unavailable(start))
}

pub(crate) fn client_ip(request_headers: &axum::http::HeaderMap) -> Option<String> {
    for header in ["x-forwarded-for", "x-real-ip"] {
        if let Some(value) = request_headers.get(header).and_then(|v| v.to_str().ok()) {
            let first = value.split(',').next().unwrap_or("").trim();
            if !first.is_empty() {
                return Some(first.to_string());
            }
        }
    }
    None
}

fn user_agent(request_headers: &axum::http::HeaderMap) -> Option<String> {
    request_headers
        .get("user-agent")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.chars().take(255).collect())
}

pub(crate) fn user_json(user: &User) -> Value {
    json!({
        "id": user.id,
        "uuid": user.uuid,
        "email_address": user.email_address,
        "first_name": user.first_name,
        "last_name": user.last_name,
        "admin": user.admin,
    })
}

pub(crate) async fn audit(
    store: &Arc<dyn AuthStore>,
    user_id: Option<camelmailer_core::Id>,
    email: Option<&str>,
    event: &str,
    headers: &axum::http::HeaderMap,
) {
    let _ = store
        .record_auth_event(NewAuthEvent {
            user_id,
            email_address: email.map(str::to_string),
            event: event.to_string(),
            ip_address: client_ip(headers),
            user_agent: user_agent(headers),
        })
        .await;
}

// ------------------------------------------------------------------ login

#[derive(Debug, Deserialize)]
struct LoginBody {
    email_address: Option<String>,
    password: Option<String>,
    totp_code: Option<String>,
}

async fn login(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Json(body): Json<LoginBody>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let Some(email) = body.email_address.filter(|e| !e.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: email_address",
        );
    };
    let Some(password) = body.password.filter(|p| !p.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: password",
        );
    };

    let invalid = |start: &RequestStart| {
        render_error(
            Some(start),
            StatusCode::UNAUTHORIZED,
            "InvalidCredentials",
            "The email address or password is incorrect",
        )
    };

    let user = match store.user_by_email(&email).await {
        Ok(user) => user,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(user) = user else {
        // burn the same time as a real verification, then fail
        auth::verify_password(&password, dummy_digest());
        audit(store, None, Some(&email), "login.failure", &headers).await;
        return invalid(&start.0);
    };

    let user_auth = match store.user_auth(user.id).await {
        Ok(Some(user_auth)) => user_auth,
        Ok(None) => return invalid(&start.0),
        Err(error) => return render_store_error(Some(&start.0), error),
    };

    if user_auth.disabled {
        audit(
            store,
            Some(user.id),
            Some(&email),
            "login.disabled",
            &headers,
        )
        .await;
        return render_error(
            Some(&start.0),
            StatusCode::FORBIDDEN,
            "AccountDisabled",
            "This account has been deactivated",
        );
    }

    let now = Utc::now();
    if let Some(locked_until) = user_auth.locked_until {
        if locked_until > now {
            audit(store, Some(user.id), Some(&email), "login.locked", &headers).await;
            return render_error(
                Some(&start.0),
                StatusCode::FORBIDDEN,
                "AccountLocked",
                "This account is temporarily locked after repeated failed logins",
            );
        }
    }

    let policy = &state.config.auth;
    let password_ok = user_auth
        .password_digest
        .as_deref()
        .map(|digest| auth::verify_password(&password, digest))
        .unwrap_or_else(|| {
            auth::verify_password(&password, dummy_digest());
            false
        });
    if !password_ok {
        let attempts = user_auth.failed_login_attempts + 1;
        let locked_until = (attempts >= policy.max_login_attempts)
            .then(|| now + Duration::minutes(policy.lockout_minutes as i64));
        let _ = store
            .set_login_state(user.id, attempts, locked_until, None)
            .await;
        audit(
            store,
            Some(user.id),
            Some(&email),
            "login.failure",
            &headers,
        )
        .await;
        return invalid(&start.0);
    }

    if user_auth.totp_enabled {
        let secret = user_auth.totp_secret.as_deref().unwrap_or("");
        match body.totp_code.as_deref().filter(|c| !c.is_empty()) {
            None => {
                return render_error(
                    Some(&start.0),
                    StatusCode::UNAUTHORIZED,
                    "TOTPRequired",
                    "A two-factor authentication code is required",
                )
            }
            Some(code) => {
                if !auth::verify_totp(secret, code, now.timestamp() as u64) {
                    let attempts = user_auth.failed_login_attempts + 1;
                    let locked_until = (attempts >= policy.max_login_attempts)
                        .then(|| now + Duration::minutes(policy.lockout_minutes as i64));
                    let _ = store
                        .set_login_state(user.id, attempts, locked_until, None)
                        .await;
                    audit(
                        store,
                        Some(user.id),
                        Some(&email),
                        "login.totp_failure",
                        &headers,
                    )
                    .await;
                    return render_error(
                        Some(&start.0),
                        StatusCode::UNAUTHORIZED,
                        "InvalidTOTPCode",
                        "The two-factor authentication code is incorrect",
                    );
                }
            }
        }
    }

    if let Err(error) = store.set_login_state(user.id, 0, None, Some(now)).await {
        return render_store_error(Some(&start.0), error);
    }
    match issue_session(store, &state, &user, &headers).await {
        Ok((token, session)) => {
            audit(
                store,
                Some(user.id),
                Some(&email),
                "login.success",
                &headers,
            )
            .await;
            render_success(
                Some(&start.0),
                StatusCode::CREATED,
                json!({
                    "session_token": token,
                    "expires_at": session.expires_at,
                    "user": user_json(&user),
                }),
            )
        }
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

// --------------------------------------------------------- registration

#[derive(Debug, Deserialize)]
struct RegisterBody {
    email_address: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    password: Option<String>,
}

/// `POST /api/v2/auth/register` — open self-registration. Gated behind
/// `auth.allow_registration` (off by default); creates a regular
/// (non-admin) account and signs it in, mirroring the login success
/// response. A fresh account never has 2FA enabled.
async fn register(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Json(body): Json<RegisterBody>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    if !state.config.auth.allow_registration {
        return render_error(
            Some(&start.0),
            StatusCode::FORBIDDEN,
            "RegistrationDisabled",
            "Self-registration is disabled on this instance",
        );
    }
    let Some(email) = body.email_address.filter(|e| !e.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: email_address",
        );
    };
    let Some(first_name) = body.first_name.filter(|v| !v.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: first_name",
        );
    };
    let Some(last_name) = body.last_name.filter(|v| !v.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: last_name",
        );
    };
    let Some(password) = body.password.filter(|p| !p.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: password",
        );
    };
    if !email.contains('@') {
        return render_validation_error(Some(&start.0), "Email address is invalid");
    }
    if (password.len() as u32) < state.config.auth.minimum_password_length {
        return render_validation_error(
            Some(&start.0),
            &format!(
                "Password must be at least {} characters",
                state.config.auth.minimum_password_length
            ),
        );
    }
    // A duplicate address surfaces as StoreError::Conflict from the store
    // ("Email address has already been taken") → 422 ValidationError.
    let user = match state
        .store
        .create_user(camelmailer_core::NewUser {
            email_address: email,
            first_name,
            last_name,
            admin: false,
        })
        .await
    {
        Ok(user) => user,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let digest = match auth::hash_password(&password) {
        Ok(digest) => digest,
        Err(error) => return render_store_error(Some(&start.0), StoreError::Other(error)),
    };
    if let Err(error) = store.set_password_digest(user.id, &digest).await {
        return render_store_error(Some(&start.0), error);
    }
    audit(
        store,
        Some(user.id),
        Some(&user.email_address),
        "registration.success",
        &headers,
    )
    .await;
    // Welcome mail (no token) — a no-op unless app_mail is enabled; a
    // delivery failure is logged and never fails the registration.
    crate::app_mailer::deliver(
        &state,
        crate::app_mailer::welcome_mail(
            &user.email_address,
            &user.first_name,
            state.config.auth.frontend_url.as_deref(),
        ),
    )
    .await;
    // Starter workspace (auth.bootstrap_workspace, cloud): org + two servers +
    // one API credential whose key appears exactly once, right here. A
    // bootstrap failure is logged and never fails the registration.
    let workspace = crate::workspace::bootstrap_workspace(&state, &user, true).await;
    match issue_session(store, &state, &user, &headers).await {
        Ok((token, session)) => {
            let mut data = json!({
                "session_token": token,
                "expires_at": session.expires_at,
                "user": user_json(&user),
            });
            if let Some(workspace) = workspace {
                data["workspace"] = json!({
                    "organization": workspace.organization.permalink,
                    "server": workspace.server.permalink,
                    // the one time the full API key is returned
                    "api_key": workspace.api_key,
                });
            }
            render_success(Some(&start.0), StatusCode::CREATED, data)
        }
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

pub(crate) async fn issue_session(
    store: &Arc<dyn AuthStore>,
    state: &ApiState,
    user: &User,
    headers: &axum::http::HeaderMap,
) -> Result<(String, AuthSession), StoreError> {
    let token = auth::generate_auth_token();
    let expires_at = Utc::now() + Duration::days(state.config.auth.session_timeout_days as i64);
    let session = store
        .create_session(NewAuthSession {
            user_id: user.id,
            token_hash: auth::hash_token(&token),
            expires_at,
            ip_address: client_ip(headers),
            user_agent: user_agent(headers),
        })
        .await?;
    Ok((token, session))
}

// ------------------------------------------------- session middleware

/// Resolve `Authorization: Bearer <token>` into a [`CurrentUser`]
/// extension. Sessions slide: each authenticated request pushes
/// `expires_at` forward by the configured timeout.
pub(crate) async fn session_middleware(
    State(state): State<Arc<ApiState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let start = request.extensions().get::<RequestStart>().copied();
    let Some(store) = state.auth_store.clone() else {
        return unavailable(start.as_ref()).into_response();
    };
    let token = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .trim()
        .to_string();
    if token.is_empty() {
        return render_error(
            start.as_ref(),
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "Missing Authorization: Bearer session token",
        )
        .into_response();
    }
    let token_hash = auth::hash_token(&token);
    let session = match store.session_with_user(&token_hash).await {
        Ok(session) => session,
        Err(error) => return render_store_error(start.as_ref(), error).into_response(),
    };
    let now = Utc::now();
    let Some((session, user)) = session.filter(|(session, _)| session.expires_at > now) else {
        return render_error(
            start.as_ref(),
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The session token is invalid or has expired",
        )
        .into_response();
    };
    let _ = store
        .touch_session(
            session.id,
            now,
            now + Duration::days(state.config.auth.session_timeout_days as i64),
        )
        .await;
    request
        .extensions_mut()
        .insert(CurrentUser { user, token_hash });
    next.run(request).await
}

// ------------------------------------------------------- authenticated

// Logout is local session revocation only. RP-initiated OIDC single-logout
// (redirecting to the IdP `end_session_endpoint` with an `id_token_hint`) is
// deliberately out of scope: this is a JSON API the dashboard calls via fetch,
// not a browser navigation, and sessions do not retain the originating
// id_token or issuer — a correct SLO flow would need a session-schema change,
// a discovery `end_session_endpoint` lookup, and a redirecting GET endpoint.
// Killing the local session already blocks all further access here.
async fn logout(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
    headers: axum::http::HeaderMap,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    if let Err(error) = store.delete_session(&current.token_hash).await {
        return render_store_error(Some(&start.0), error);
    }
    audit(
        store,
        Some(current.user.id),
        Some(&current.user.email_address),
        "logout",
        &headers,
    )
    .await;
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "logged_out": true }),
    )
}

async fn me(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let totp_enabled = match store.user_auth(current.user.id).await {
        Ok(user_auth) => user_auth.map(|a| a.totp_enabled).unwrap_or(false),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let memberships = match store.memberships_for_user(current.user.id).await {
        Ok(memberships) => memberships,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let memberships: Vec<Value> = memberships
        .iter()
        .map(|(membership, organization)| {
            json!({
                "role": membership.role.as_str(),
                "organization": {
                    "id": organization.id,
                    "uuid": organization.uuid,
                    "name": organization.name,
                    "permalink": organization.permalink,
                },
            })
        })
        .collect();
    let mut user = user_json(&current.user);
    user["totp_enabled"] = json!(totp_enabled);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "user": user, "memberships": memberships }),
    )
}

#[derive(Debug, Deserialize)]
struct UpdateMe {
    first_name: Option<String>,
    last_name: Option<String>,
}

async fn update_me(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
    Json(body): Json<UpdateMe>,
) -> ApiResponse {
    let mut user = current.user.clone();
    if let Some(first_name) = body.first_name.filter(|v| !v.is_empty()) {
        user.first_name = first_name;
    }
    if let Some(last_name) = body.last_name.filter(|v| !v.is_empty()) {
        user.last_name = last_name;
    }
    match state.store.update_user(user).await {
        Ok(user) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "user": user_json(&user) }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize)]
struct ChangePassword {
    current_password: Option<String>,
    new_password: Option<String>,
}

async fn change_password(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ChangePassword>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let Some(new_password) = body.new_password.filter(|p| !p.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: new_password",
        );
    };
    if (new_password.len() as u32) < state.config.auth.minimum_password_length {
        return render_validation_error(
            Some(&start.0),
            &format!(
                "Password must be at least {} characters",
                state.config.auth.minimum_password_length
            ),
        );
    }
    let user_auth = match store.user_auth(current.user.id).await {
        Ok(Some(user_auth)) => user_auth,
        Ok(None) => {
            return render_error(
                Some(&start.0),
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "Unknown user",
            )
        }
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    if let Some(digest) = user_auth.password_digest.as_deref() {
        let current_ok = body
            .current_password
            .as_deref()
            .map(|p| auth::verify_password(p, digest))
            .unwrap_or(false);
        if !current_ok {
            return render_error(
                Some(&start.0),
                StatusCode::UNAUTHORIZED,
                "InvalidCredentials",
                "The current password is incorrect",
            );
        }
    }
    let digest = match auth::hash_password(&new_password) {
        Ok(digest) => digest,
        Err(error) => return render_store_error(Some(&start.0), StoreError::Other(error)),
    };
    if let Err(error) = store.set_password_digest(current.user.id, &digest).await {
        return render_store_error(Some(&start.0), error);
    }
    // Revoke every session (including this one) and hand back a fresh
    // token so other devices are logged out by a password change.
    let _ = store.delete_sessions_for_user(current.user.id).await;
    audit(
        store,
        Some(current.user.id),
        Some(&current.user.email_address),
        "password.change",
        &headers,
    )
    .await;
    match issue_session(store, &state, &current.user, &headers).await {
        Ok((token, session)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "password_changed": true,
                "session_token": token,
                "expires_at": session.expires_at,
            }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

// ------------------------------------------------------ password reset

#[derive(Debug, Deserialize)]
struct ResetRequest {
    email_address: Option<String>,
}

async fn password_reset_request(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ResetRequest>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let Some(email) = body.email_address.filter(|e| !e.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: email_address",
        );
    };
    // Deliberately identical response whether or not the account exists
    // (or has been deactivated — no token is issued for disabled ones).
    if let Ok(Some(user)) = store.user_by_email(&email).await {
        let disabled = matches!(
            store.user_auth(user.id).await,
            Ok(Some(user_auth)) if user_auth.disabled
        );
        if disabled {
            return render_success(
                Some(&start.0),
                StatusCode::OK,
                json!({ "reset_requested": true }),
            );
        }
        let token = auth::generate_auth_token();
        let expires_at =
            Utc::now() + Duration::hours(state.config.auth.password_reset_expiry_hours as i64);
        let _ = store
            .create_password_reset(user.id, &auth::hash_token(&token), expires_at)
            .await;
        audit(
            store,
            Some(user.id),
            Some(&email),
            "password.reset_request",
            &headers,
        )
        .await;
        let link = state.config.auth.frontend_url.as_deref().map(|base| {
            format!(
                "{}/reset-password?token={}",
                base.trim_end_matches('/'),
                token
            )
        });
        // With app_mail enabled the reset link is emailed to the user (and
        // the token stays out of the log); otherwise — or when the mail
        // cannot be enqueued — it is logged for the operator to relay.
        let mut mailed = false;
        if state.config.app_mail.enabled {
            match link.as_deref() {
                Some(link) => {
                    mailed = crate::app_mailer::deliver(
                        &state,
                        crate::app_mailer::password_reset_mail(
                            &email,
                            link,
                            state.config.auth.password_reset_expiry_hours,
                        ),
                    )
                    .await;
                }
                None => tracing::warn!(
                    "app_mail is enabled but auth.frontend_url is not set; cannot email the reset link"
                ),
            }
        }
        if mailed {
            tracing::info!(user = %email, "password reset requested; reset link emailed");
        } else {
            tracing::info!(
                user = %email,
                link = link.as_deref().unwrap_or("(configure auth.frontend_url for a link)"),
                token = %token,
                "password reset requested"
            );
        }
    }
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "reset_requested": true }),
    )
}

#[derive(Debug, Deserialize)]
struct ResetComplete {
    token: Option<String>,
    new_password: Option<String>,
}

async fn password_reset_complete(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Json(body): Json<ResetComplete>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let Some(token) = body.token.filter(|t| !t.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: token",
        );
    };
    let Some(new_password) = body.new_password.filter(|p| !p.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: new_password",
        );
    };
    if (new_password.len() as u32) < state.config.auth.minimum_password_length {
        return render_validation_error(
            Some(&start.0),
            &format!(
                "Password must be at least {} characters",
                state.config.auth.minimum_password_length
            ),
        );
    }
    let user_id = match store
        .consume_password_reset(&auth::hash_token(&token), Utc::now())
        .await
    {
        Ok(Some(user_id)) => user_id,
        Ok(None) => {
            return render_error(
                Some(&start.0),
                StatusCode::UNPROCESSABLE_ENTITY,
                "InvalidToken",
                "The reset token is invalid or has expired",
            )
        }
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    // A deactivated account cannot regain access through a reset link
    // issued before the deactivation.
    let disabled = matches!(
        store.user_auth(user_id).await,
        Ok(Some(user_auth)) if user_auth.disabled
    );
    if disabled {
        return render_error(
            Some(&start.0),
            StatusCode::FORBIDDEN,
            "AccountDisabled",
            "This account has been deactivated",
        );
    }
    let digest = match auth::hash_password(&new_password) {
        Ok(digest) => digest,
        Err(error) => return render_store_error(Some(&start.0), StoreError::Other(error)),
    };
    if let Err(error) = store.set_password_digest(user_id, &digest).await {
        return render_store_error(Some(&start.0), error);
    }
    let _ = store.delete_sessions_for_user(user_id).await;
    let _ = store.set_login_state(user_id, 0, None, None).await;
    audit(
        store,
        Some(user_id),
        None,
        "password.reset_complete",
        &headers,
    )
    .await;
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "password_reset": true }),
    )
}

// --------------------------------------------------------------- TOTP

async fn totp_enroll(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let secret = auth::generate_totp_secret();
    if let Err(error) = store.set_totp(current.user.id, Some(&secret), false).await {
        return render_store_error(Some(&start.0), error);
    }
    let issuer = format!("CamelMailer ({})", state.config.camelmailer.web_hostname);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "secret": secret,
            "otpauth_url": auth::otpauth_url(&secret, &current.user.email_address, &issuer),
        }),
    )
}

#[derive(Debug, Deserialize)]
struct TotpActivate {
    code: Option<String>,
}

async fn totp_activate(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
    headers: axum::http::HeaderMap,
    Json(body): Json<TotpActivate>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let Some(code) = body.code.filter(|c| !c.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: code",
        );
    };
    let user_auth = match store.user_auth(current.user.id).await {
        Ok(Some(user_auth)) => user_auth,
        Ok(None) | Err(_) => {
            return render_validation_error(Some(&start.0), "TOTP enrollment has not been started")
        }
    };
    let Some(secret) = user_auth.totp_secret else {
        return render_validation_error(Some(&start.0), "TOTP enrollment has not been started");
    };
    if !auth::verify_totp(&secret, &code, Utc::now().timestamp() as u64) {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "InvalidTOTPCode",
            "The two-factor authentication code is incorrect",
        );
    }
    if let Err(error) = store.set_totp(current.user.id, Some(&secret), true).await {
        return render_store_error(Some(&start.0), error);
    }
    audit(
        store,
        Some(current.user.id),
        Some(&current.user.email_address),
        "totp.activate",
        &headers,
    )
    .await;
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "totp_enabled": true }),
    )
}

#[derive(Debug, Deserialize)]
struct TotpDisable {
    password: Option<String>,
}

async fn totp_disable(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    current: axum::Extension<CurrentUser>,
    headers: axum::http::HeaderMap,
    Json(body): Json<TotpDisable>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let user_auth = match store.user_auth(current.user.id).await {
        Ok(Some(user_auth)) => user_auth,
        Ok(None) => return render_validation_error(Some(&start.0), "TOTP is not enabled"),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    // Disabling a second factor requires the password (when one is set).
    if let Some(digest) = user_auth.password_digest.as_deref() {
        let password_ok = body
            .password
            .as_deref()
            .map(|p| auth::verify_password(p, digest))
            .unwrap_or(false);
        if !password_ok {
            return render_error(
                Some(&start.0),
                StatusCode::UNAUTHORIZED,
                "InvalidCredentials",
                "The password is incorrect",
            );
        }
    }
    if let Err(error) = store.set_totp(current.user.id, None, false).await {
        return render_store_error(Some(&start.0), error);
    }
    audit(
        store,
        Some(current.user.id),
        Some(&current.user.email_address),
        "totp.disable",
        &headers,
    )
    .await;
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "totp_enabled": false }),
    )
}

// -------------------------------------------------------- invitations

/// `GET /api/v2/auth/invitations/{token}` — preview an invitation for the
/// accept page: which organization, which address, which role, and
/// whether an account already exists for the address.
async fn invitation_preview(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let invitation = match store
        .invitation_by_token_hash(&auth::hash_token(&token))
        .await
    {
        Ok(Some(invitation)) => invitation,
        Ok(None) => return invalid_invitation(&start.0),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    if invitation.accepted_at.is_some() || invitation.expires_at < Utc::now() {
        return invalid_invitation(&start.0);
    }
    let organization = match state.store.list_organizations().await {
        Ok(organizations) => organizations
            .into_iter()
            .find(|organization| organization.id == invitation.organization_id),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let user_exists = matches!(
        store.user_by_email(&invitation.email_address).await,
        Ok(Some(_))
    );
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "invitation": {
                "email_address": invitation.email_address,
                "role": invitation.role.as_str(),
                "expires_at": invitation.expires_at,
                "organization": organization.map(|organization| json!({
                    "name": organization.name,
                    "permalink": organization.permalink,
                })),
                "user_exists": user_exists,
            },
        }),
    )
}

fn invalid_invitation(start: &RequestStart) -> ApiResponse {
    render_error(
        Some(start),
        StatusCode::UNPROCESSABLE_ENTITY,
        "InvalidToken",
        "The invitation is invalid, expired, or already used",
    )
}

#[derive(Debug, Deserialize)]
struct AcceptInvitation {
    token: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    password: Option<String>,
}

/// `POST /api/v2/auth/invitations/accept` — redeem an invitation. For a
/// new address this creates the account (name + password required) and
/// signs it in; for an existing account it only adds the membership (the
/// holder of an invite link must never gain a session for an account
/// they don't own).
async fn invitation_accept(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    headers: axum::http::HeaderMap,
    Json(body): Json<AcceptInvitation>,
) -> ApiResponse {
    let Some(store) = auth_store(&state) else {
        return unavailable(Some(&start.0));
    };
    let Some(token) = body.token.filter(|t| !t.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: token",
        );
    };
    let invitation = match store
        .invitation_by_token_hash(&auth::hash_token(&token))
        .await
    {
        Ok(Some(invitation)) => invitation,
        Ok(None) => return invalid_invitation(&start.0),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    if invitation.accepted_at.is_some() || invitation.expires_at < Utc::now() {
        return invalid_invitation(&start.0);
    }

    let existing = match store.user_by_email(&invitation.email_address).await {
        Ok(existing) => existing,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let (user, created) = match existing {
        Some(user) => (user, false),
        None => {
            let Some(password) = body.password.as_deref().filter(|p| !p.is_empty()) else {
                return render_parameter_missing(
                    Some(&start.0),
                    "param is missing or the value is empty: password",
                );
            };
            if (password.len() as u32) < state.config.auth.minimum_password_length {
                return render_validation_error(
                    Some(&start.0),
                    &format!(
                        "Password must be at least {} characters",
                        state.config.auth.minimum_password_length
                    ),
                );
            }
            let user = match state
                .store
                .create_user(camelmailer_core::NewUser {
                    email_address: invitation.email_address.clone(),
                    first_name: body.first_name.clone().unwrap_or_default(),
                    last_name: body.last_name.clone().unwrap_or_default(),
                    admin: false,
                })
                .await
            {
                Ok(user) => user,
                Err(error) => return render_store_error(Some(&start.0), error),
            };
            let digest = match auth::hash_password(password) {
                Ok(digest) => digest,
                Err(error) => return render_store_error(Some(&start.0), StoreError::Other(error)),
            };
            if let Err(error) = store.set_password_digest(user.id, &digest).await {
                return render_store_error(Some(&start.0), error);
            }
            (user, true)
        }
    };

    if let Err(error) = store
        .upsert_membership(invitation.organization_id, user.id, invitation.role)
        .await
    {
        return render_store_error(Some(&start.0), error);
    }
    if let Err(error) = store.mark_invitation_accepted(invitation.id).await {
        return render_store_error(Some(&start.0), error);
    }
    audit(
        store,
        Some(user.id),
        Some(&user.email_address),
        "invitation.accepted",
        &headers,
    )
    .await;

    let mut data = json!({
        "accepted": true,
        "user": user_json(&user),
        "account_created": created,
    });
    // Only a freshly created account is signed in by accepting.
    if created {
        match issue_session(store, &state, &user, &headers).await {
            Ok((token, session)) => {
                data["session_token"] = json!(token);
                data["expires_at"] = json!(session.expires_at);
            }
            Err(error) => return render_store_error(Some(&start.0), error),
        }
    }
    render_success(Some(&start.0), StatusCode::OK, data)
}

// ------------------------------------------------- sender addresses

#[derive(Debug, Deserialize)]
struct ConfirmSenderAddress {
    token: Option<String>,
}

/// `POST /api/v2/auth/sender-addresses/confirm` — confirm a sender
/// address via the emailed verification token. Public: the token is the
/// secret (single-use, stored hashed).
async fn sender_address_confirm(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Json(body): Json<ConfirmSenderAddress>,
) -> ApiResponse {
    let Some(token) = body.token.filter(|t| !t.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: token",
        );
    };
    match state
        .store
        .confirm_sender_address(&auth::hash_token(&token))
        .await
    {
        Ok(Some(address)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "confirmed": true, "email_address": address.email_address }),
        ),
        Ok(None) => render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "InvalidToken",
            "The confirmation token is invalid or has already been used",
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

/// `GET /api/v2/auth/features` — which optional login/registration
/// features this instance exposes; unauthenticated, so login pages can
/// decide what to render (passkey button, sign-up link, SSO button).
async fn features(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
) -> ApiResponse {
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "webauthn": state.config.auth.webauthn.enabled,
            "registration": state.config.auth.allow_registration,
            "oidc": {
                "enabled": state.config.oidc.enabled,
                "name": state.config.oidc.name,
            },
            "saml": {
                "enabled": state.config.saml.enabled,
                "name": state.config.saml.name,
            },
            "sso": state
                .config
                .auth
                .sso_providers
                .iter()
                .map(|provider| json!({
                    "id": provider.id,
                    "name": provider.name,
                    "type": provider.provider_type,
                }))
                .collect::<Vec<_>>(),
            "legal": {
                "terms_url": state.config.legal.terms_url,
                "privacy_url": state.config.legal.privacy_url,
            },
        }),
    )
}

/// Build the `/api/v2/auth` router.
pub fn build_auth_router(state: Arc<ApiState>) -> Router {
    let public = Router::new()
        .route("/login", post(login))
        .route("/register", post(register))
        .route("/features", get(features))
        .route("/password-reset", post(password_reset_request))
        .route("/password-reset/complete", post(password_reset_complete))
        .route("/invitations/accept", post(invitation_accept))
        .route("/invitations/{token}", get(invitation_preview))
        .route("/sender-addresses/confirm", post(sender_address_confirm))
        .route("/webauthn/login/start", post(crate::webauthn::login_start))
        .route(
            "/webauthn/login/finish",
            post(crate::webauthn::login_finish),
        );

    let protected = Router::new()
        .route("/logout", post(logout))
        .route("/me", get(me).patch(update_me))
        .route("/password", post(change_password))
        .route("/totp/enroll", post(totp_enroll))
        .route("/totp/activate", post(totp_activate))
        .route("/totp/disable", post(totp_disable))
        .route(
            "/webauthn/register/start",
            post(crate::webauthn::register_start),
        )
        .route(
            "/webauthn/register/finish",
            post(crate::webauthn::register_finish),
        )
        .route(
            "/webauthn/credentials",
            get(crate::webauthn::credentials_index),
        )
        .route(
            "/webauthn/credentials/{id}",
            delete(crate::webauthn::credentials_destroy),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            session_middleware,
        ));

    Router::new().nest(
        "/api/v2/auth",
        public
            .merge(protected)
            .layer(middleware::from_fn(timing_middleware))
            .with_state(state),
    )
}
