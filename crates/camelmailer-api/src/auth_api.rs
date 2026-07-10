//! `/api/v2/auth` — user accounts: password + TOTP login, sessions
//! (Bearer tokens), password change/reset, and 2FA enrollment.
//!
//! Responses use the same `{ status, time, data | error }` envelope as the
//! admin API. Error codes a frontend can branch on:
//! `InvalidCredentials`, `AccountLocked`, `TOTPRequired`,
//! `InvalidTOTPCode`, `InvalidToken`, `Unauthorized`.

use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
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

async fn audit(
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
    request.extensions_mut().insert(CurrentUser { user, token_hash });
    next.run(request).await
}

// ------------------------------------------------------- authenticated

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
    // Deliberately identical response whether or not the account exists.
    if let Ok(Some(user)) = store.user_by_email(&email).await {
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
        // The reset link is delivered out of band; without an app-mail
        // transport it is logged for the operator to relay.
        tracing::info!(
            user = %email,
            link = link.as_deref().unwrap_or("(configure auth.frontend_url for a link)"),
            token = %token,
            "password reset requested"
        );
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

/// Build the `/api/v2/auth` router.
pub fn build_auth_router(state: Arc<ApiState>) -> Router {
    let public = Router::new()
        .route("/login", post(login))
        .route("/password-reset", post(password_reset_request))
        .route("/password-reset/complete", post(password_reset_complete));

    let protected = Router::new()
        .route("/logout", post(logout))
        .route("/me", get(me).patch(update_me))
        .route("/password", post(change_password))
        .route("/totp/enroll", post(totp_enroll))
        .route("/totp/activate", post(totp_activate))
        .route("/totp/disable", post(totp_disable))
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
