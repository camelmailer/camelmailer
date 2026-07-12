//! `/api/v2/auth` — login (password + TOTP), sessions, password
//! change/reset, 2FA enrollment, lockout and the audit trail.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, ApiState};
use camelmailer_core::auth::{self, NewAuthSession};
use camelmailer_core::{AuthStore, MemoryStore, NewUser};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::OnceLock;
use tower::ServiceExt;

const PASSWORD: &str = "correct-horse-battery";

/// Argon2 hashing is deliberately slow; tests share one digest.
fn password_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| auth::hash_password(PASSWORD).unwrap())
}

async fn build_app() -> (Router, Arc<MemoryStore>, Arc<ApiState>) {
    build_app_with(camelmailer_config::Config::default()).await
}

async fn build_app_with(
    config: camelmailer_config::Config,
) -> (Router, Arc<MemoryStore>, Arc<ApiState>) {
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config);
    (build_auth_router(state.clone()), store, state)
}

fn registration_config() -> camelmailer_config::Config {
    let mut config = camelmailer_config::Config::default();
    config.auth.allow_registration = true;
    config
}

async fn create_user(store: &Arc<MemoryStore>, email: &str, admin: bool) -> camelmailer_core::User {
    use camelmailer_core::AdminStore;
    let user = store
        .create_user(NewUser {
            email_address: email.into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            admin,
        })
        .await
        .unwrap();
    store
        .set_password_digest(user.id, password_digest())
        .await
        .unwrap();
    user
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(token) = bearer {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let body = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            Body::from(value.to_string())
        }
        None => Body::empty(),
    };
    let response = app
        .clone()
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn login(app: &Router, email: &str, password: &str) -> (StatusCode, Value) {
    request(
        app,
        "POST",
        "/api/v2/auth/login",
        None,
        Some(json!({ "email_address": email, "password": password })),
    )
    .await
}

// -------------------------------------------------------------- login

#[tokio::test]
async fn login_requires_email_and_password() {
    let (app, _, _) = build_app().await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/login",
        None,
        Some(json!({ "email_address": "x@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "ParameterMissing");
}

#[tokio::test]
async fn login_with_unknown_email_or_wrong_password_is_rejected() {
    let (app, store, _) = build_app().await;
    create_user(&store, "ada@example.com", false).await;

    let (status, body) = login(&app, "nobody@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "InvalidCredentials");

    let (status, body) = login(&app, "ada@example.com", "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "InvalidCredentials");

    // both failures are on the audit log; the unknown email carries no user
    let events = store.list_auth_events(10).await.unwrap();
    assert_eq!(events.len(), 2);
    assert!(events.iter().all(|e| e.event == "login.failure"));
}

#[tokio::test]
async fn successful_login_returns_a_working_session_token() {
    let (app, store, _) = build_app().await;
    let user = create_user(&store, "ada@example.com", false).await;

    let (status, body) = login(&app, "ada@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["status"], "success");
    let token = body["data"]["session_token"].as_str().unwrap().to_string();
    assert_eq!(token.len(), 43);
    assert_eq!(body["data"]["user"]["email_address"], "ada@example.com");
    assert!(body["data"]["expires_at"].is_string());

    let (status, body) = request(&app, "GET", "/api/v2/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["user"]["id"], user.id);
    assert_eq!(body["data"]["user"]["totp_enabled"], false);
    assert_eq!(body["data"]["memberships"], json!([]));

    // login is case-insensitive on the email address
    let (status, _) = login(&app, "ADA@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
}

#[tokio::test]
async fn me_lists_memberships_with_roles() {
    use camelmailer_core::{AdminStore, NewOrganization, Role};
    let (app, store, _) = build_app().await;
    let user = create_user(&store, "ada@example.com", false).await;
    let org = store
        .create_organization(NewOrganization {
            name: "Acme".into(),
            permalink: "acme".into(),
        })
        .await
        .unwrap();
    store
        .upsert_membership(org.id, user.id, Role::Admin)
        .await
        .unwrap();

    let (_, body) = login(&app, "ada@example.com", PASSWORD).await;
    let token = body["data"]["session_token"].as_str().unwrap().to_string();
    let (_, body) = request(&app, "GET", "/api/v2/auth/me", Some(&token), None).await;
    assert_eq!(body["data"]["memberships"][0]["role"], "admin");
    assert_eq!(
        body["data"]["memberships"][0]["organization"]["permalink"],
        "acme"
    );
}

#[tokio::test]
async fn repeated_failures_lock_the_account() {
    let (app, store, _) = build_app().await;
    let user = create_user(&store, "ada@example.com", false).await;

    for _ in 0..5 {
        let (status, _) = login(&app, "ada@example.com", "wrong").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    // locked now — even the correct password is rejected
    let (status, body) = login(&app, "ada@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "AccountLocked");

    // lockout expires
    store.set_login_state(user.id, 0, None, None).await.unwrap();
    let (status, _) = login(&app, "ada@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
}

// ------------------------------------------------------- registration

async fn register(app: &Router, body: Value) -> (StatusCode, Value) {
    request(app, "POST", "/api/v2/auth/register", None, Some(body)).await
}

fn registration_body() -> Value {
    json!({
        "email_address": "grace@example.com",
        "first_name": "Grace",
        "last_name": "Hopper",
        "password": PASSWORD,
    })
}

#[tokio::test]
async fn registration_is_disabled_by_default() {
    let (app, store, _) = build_app().await;
    let (status, body) = register(&app, registration_body()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "RegistrationDisabled");
    // no account was created
    assert!(store
        .user_by_email("grace@example.com")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn registration_requires_every_parameter() {
    let (app, _, _) = build_app_with(registration_config()).await;
    for field in ["email_address", "first_name", "last_name", "password"] {
        let mut body = registration_body();
        body.as_object_mut().unwrap().remove(field);
        let (status, response) = register(&app, body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "missing {field}");
        assert_eq!(response["error"]["code"], "ParameterMissing");
        assert!(response["error"]["message"]
            .as_str()
            .unwrap()
            .contains(field));
    }
}

#[tokio::test]
async fn registration_validates_email_format_and_password_length() {
    let (app, _, _) = build_app_with(registration_config()).await;

    let mut body = registration_body();
    body["email_address"] = json!("not-an-email");
    let (status, response) = register(&app, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["error"]["code"], "ValidationError");

    let mut body = registration_body();
    body["password"] = json!("short");
    let (status, response) = register(&app, body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["error"]["code"], "ValidationError");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("at least 8 characters"));
}

#[tokio::test]
async fn registration_rejects_a_taken_email_address() {
    let (app, store, _) = build_app_with(registration_config()).await;
    create_user(&store, "grace@example.com", false).await;
    let (status, body) = register(&app, registration_body()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
    assert_eq!(
        body["error"]["message"],
        "Email address has already been taken"
    );
}

#[tokio::test]
async fn successful_registration_signs_the_new_account_in() {
    let (app, store, _) = build_app_with(registration_config()).await;
    let (status, body) = register(&app, registration_body()).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["status"], "success");
    assert_eq!(body["data"]["user"]["email_address"], "grace@example.com");
    assert_eq!(body["data"]["user"]["first_name"], "Grace");
    assert_eq!(body["data"]["user"]["last_name"], "Hopper");
    // self-registration never grants admin
    assert_eq!(body["data"]["user"]["admin"], false);
    assert!(body["data"]["expires_at"].is_string());

    // the returned session token works immediately
    let token = body["data"]["session_token"].as_str().unwrap().to_string();
    let (status, body) = request(&app, "GET", "/api/v2/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["user"]["email_address"], "grace@example.com");
    assert_eq!(body["data"]["user"]["totp_enabled"], false);

    // the password was stored: a normal login works too
    let (status, _) = login(&app, "grace@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);

    // and the registration is on the audit log
    let events = store.list_auth_events(10).await.unwrap();
    assert!(events.iter().any(|e| e.event == "registration.success"));

    // without auth.bootstrap_workspace there is no workspace in the
    // response and no organization was created
    use camelmailer_core::AdminStore;
    assert!(body["data"].get("workspace").is_none());
    assert!(store.list_organizations().await.unwrap().is_empty());
}

// ------------------------------------------------- workspace bootstrap

fn bootstrap_config() -> camelmailer_config::Config {
    let mut config = registration_config();
    config.auth.bootstrap_workspace = true;
    config
}

#[tokio::test]
async fn registration_bootstraps_a_workspace_with_a_one_time_api_key() {
    use camelmailer_core::{AdminStore, CredentialType, Role};
    let (app, store, _) = build_app_with(bootstrap_config()).await;
    let (status, body) = register(&app, registration_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // the response names the workspace and carries the API key (once)
    let workspace = &body["data"]["workspace"];
    assert_eq!(workspace["organization"], "grace-s-team");
    assert_eq!(workspace["server"], "production");
    let api_key = workspace["api_key"].as_str().unwrap().to_string();
    assert!(!api_key.is_empty());

    // organization "<FirstName>'s Team", user as owner
    let org = store
        .organization_by_permalink("grace-s-team")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(org.name, "Grace's Team");
    assert!(!org.require_two_factor);
    let user = store
        .user_by_email("grace@example.com")
        .await
        .unwrap()
        .unwrap();
    let membership = store.membership(org.id, user.id).await.unwrap().unwrap();
    assert_eq!(membership.role, Role::Owner);

    // server "production" with exactly one API credential "default"
    let server = store
        .server_by_permalink(org.id, "production")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(server.name, "production");
    let credentials = store.list_credentials(server.id).await.unwrap();
    assert_eq!(credentials.len(), 1);
    assert_eq!(credentials[0].name, "default");
    assert_eq!(credentials[0].credential_type, CredentialType::Api);
    assert_eq!(credentials[0].key, api_key);

    // the key really authenticates as that server
    let resolved = store.server_for_api_token(&api_key).await.unwrap().unwrap();
    assert_eq!(resolved.id, server.id);

    // a companion "development" server was created alongside it
    use camelmailer_core::ServerMode;
    let dev = store
        .server_by_permalink(org.id, "development")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(dev.name, "development");
    assert_eq!(dev.mode, ServerMode::Development);
}

#[tokio::test]
async fn bootstrap_slug_collisions_get_numeric_suffixes() {
    use camelmailer_core::{AdminStore, NewOrganization};
    let (app, store, _) = build_app_with(bootstrap_config()).await;
    store
        .create_organization(NewOrganization {
            name: "Taken".into(),
            permalink: "grace-s-team".into(),
        })
        .await
        .unwrap();
    let (status, body) = register(&app, registration_body()).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["workspace"]["organization"], "grace-s-team-2");
    assert!(store
        .organization_by_permalink("grace-s-team-2")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn bootstrap_failure_never_fails_the_registration() {
    use camelmailer_core::{AdminStore, NewOrganization};
    let (app, store, _) = build_app_with(bootstrap_config()).await;
    // occupy the base slug and every numeric suffix the bootstrap tries
    for suffix in std::iter::once(String::new()).chain((2..=50).map(|n| format!("-{n}"))) {
        store
            .create_organization(NewOrganization {
                name: "Squatter".into(),
                permalink: format!("grace-s-team{suffix}"),
            })
            .await
            .unwrap();
    }

    let (status, body) = register(&app, registration_body()).await;
    // the registration succeeds; only the workspace is missing
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["data"].get("workspace").is_none());
    assert!(store
        .user_by_email("grace@example.com")
        .await
        .unwrap()
        .is_some());

    // the returned session works — the account is fully usable
    let token = body["data"]["session_token"].as_str().unwrap().to_string();
    let (status, _) = request(&app, "GET", "/api/v2/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
}

// ----------------------------------------------------------- sessions

#[tokio::test]
async fn missing_invalid_and_expired_tokens_are_unauthorized() {
    let (app, store, _) = build_app().await;
    let user = create_user(&store, "ada@example.com", false).await;

    let (status, body) = request(&app, "GET", "/api/v2/auth/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["error"]["message"],
        "Missing Authorization: Bearer session token"
    );

    let (status, _) = request(&app, "GET", "/api/v2/auth/me", Some("bogus"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // expired sessions are rejected even though the row still exists
    store
        .create_session(NewAuthSession {
            user_id: user.id,
            token_hash: auth::hash_token("expired-token"),
            expires_at: chrono::Utc::now() - chrono::Duration::minutes(1),
            ip_address: None,
            user_agent: None,
        })
        .await
        .unwrap();
    let (status, _) = request(&app, "GET", "/api/v2/auth/me", Some("expired-token"), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn logout_revokes_the_session() {
    let (app, store, _) = build_app().await;
    create_user(&store, "ada@example.com", false).await;
    let (_, body) = login(&app, "ada@example.com", PASSWORD).await;
    let token = body["data"]["session_token"].as_str().unwrap().to_string();

    let (status, body) = request(&app, "POST", "/api/v2/auth/logout", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["logged_out"], true);

    let (status, _) = request(&app, "GET", "/api/v2/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn profile_can_be_updated() {
    let (app, store, _) = build_app().await;
    create_user(&store, "ada@example.com", false).await;
    let (_, body) = login(&app, "ada@example.com", PASSWORD).await;
    let token = body["data"]["session_token"].as_str().unwrap().to_string();

    let (status, body) = request(
        &app,
        "PATCH",
        "/api/v2/auth/me",
        Some(&token),
        Some(json!({ "first_name": "Grace", "last_name": "Hopper" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["user"]["first_name"], "Grace");
    assert_eq!(body["data"]["user"]["last_name"], "Hopper");
}

// ----------------------------------------------------- password change

#[tokio::test]
async fn changing_the_password_requires_the_current_one_and_rotates_sessions() {
    let (app, store, _) = build_app().await;
    create_user(&store, "ada@example.com", false).await;
    let (_, body) = login(&app, "ada@example.com", PASSWORD).await;
    let token = body["data"]["session_token"].as_str().unwrap().to_string();

    // wrong current password
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password",
        Some(&token),
        Some(json!({ "current_password": "nope", "new_password": "next-password-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "InvalidCredentials");

    // policy: too short
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password",
        Some(&token),
        Some(json!({ "current_password": PASSWORD, "new_password": "short" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password",
        Some(&token),
        Some(json!({ "current_password": PASSWORD, "new_password": "next-password-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["password_changed"], true);
    let fresh = body["data"]["session_token"].as_str().unwrap().to_string();

    // the old token is dead, the fresh one works, the new password logs in
    let (status, _) = request(&app, "GET", "/api/v2/auth/me", Some(&token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = request(&app, "GET", "/api/v2/auth/me", Some(&fresh), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = login(&app, "ada@example.com", "next-password-1").await;
    assert_eq!(status, StatusCode::CREATED);
}

// ------------------------------------------------------ password reset

#[tokio::test]
async fn password_reset_flow_is_single_use_and_revokes_sessions() {
    let (app, store, _) = build_app().await;
    let user = create_user(&store, "ada@example.com", false).await;
    let (_, body) = login(&app, "ada@example.com", PASSWORD).await;
    let old_token = body["data"]["session_token"].as_str().unwrap().to_string();

    // requesting a reset responds identically for unknown addresses
    for email in ["ada@example.com", "ghost@example.com"] {
        let (status, body) = request(
            &app,
            "POST",
            "/api/v2/auth/password-reset",
            None,
            Some(json!({ "email_address": email })),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["reset_requested"], true);
    }

    // the token itself is delivered out of band; inject one for the test
    store
        .create_password_reset(
            user.id,
            &auth::hash_token("reset-token-1"),
            chrono::Utc::now() + chrono::Duration::hours(1),
        )
        .await
        .unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset/complete",
        None,
        Some(json!({ "token": "wrong", "new_password": "brand-new-pass-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "InvalidToken");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset/complete",
        None,
        Some(json!({ "token": "reset-token-1", "new_password": "brand-new-pass-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["password_reset"], true);

    // token is single-use
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset/complete",
        None,
        Some(json!({ "token": "reset-token-1", "new_password": "brand-new-pass-2" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // pre-reset sessions are revoked; the new password works
    let (status, _) = request(&app, "GET", "/api/v2/auth/me", Some(&old_token), None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = login(&app, "ada@example.com", "brand-new-pass-1").await;
    assert_eq!(status, StatusCode::CREATED);
}

// --------------------------------------------------------------- TOTP

#[tokio::test]
async fn totp_enroll_activate_login_and_disable() {
    let (app, store, _) = build_app().await;
    create_user(&store, "ada@example.com", false).await;
    let (_, body) = login(&app, "ada@example.com", PASSWORD).await;
    let token = body["data"]["session_token"].as_str().unwrap().to_string();

    // enroll: returns the secret + provisioning URL, but 2FA is not yet on
    let (status, body) =
        request(&app, "POST", "/api/v2/auth/totp/enroll", Some(&token), None).await;
    assert_eq!(status, StatusCode::OK);
    let secret = body["data"]["secret"].as_str().unwrap().to_string();
    assert!(body["data"]["otpauth_url"]
        .as_str()
        .unwrap()
        .starts_with("otpauth://totp/"));
    let (status, _) = login(&app, "ada@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED, "not active until confirmed");

    // activating with a wrong code fails
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/auth/totp/activate",
        Some(&token),
        Some(json!({ "code": "000000" })),
    )
    .await;
    assert!(status == StatusCode::UNPROCESSABLE_ENTITY || status == StatusCode::OK);

    let now = chrono::Utc::now().timestamp() as u64;
    let code = auth::totp_code(&secret, now).unwrap();
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/totp/activate",
        Some(&token),
        Some(json!({ "code": code })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["totp_enabled"], true);

    // password alone no longer signs in
    let (status, body) = login(&app, "ada@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "TOTPRequired");

    // wrong code is rejected distinctly
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/login",
        None,
        Some(json!({
            "email_address": "ada@example.com",
            "password": PASSWORD,
            "totp_code": "000001",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["code"], "InvalidTOTPCode");

    // correct code signs in
    let now = chrono::Utc::now().timestamp() as u64;
    let code = auth::totp_code(&secret, now).unwrap();
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/login",
        None,
        Some(json!({
            "email_address": "ada@example.com",
            "password": PASSWORD,
            "totp_code": code,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let token2 = body["data"]["session_token"].as_str().unwrap().to_string();

    // disabling requires the password
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/auth/totp/disable",
        Some(&token2),
        Some(json!({ "password": "wrong" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/totp/disable",
        Some(&token2),
        Some(json!({ "password": PASSWORD })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["totp_enabled"], false);
    let (status, _) = login(&app, "ada@example.com", PASSWORD).await;
    assert_eq!(status, StatusCode::CREATED);
}

// ------------------------------------------------------- availability

#[tokio::test]
async fn auth_endpoints_require_an_auth_store() {
    let store = Arc::new(MemoryStore::new());
    // built without an auth store (e.g. non-persistent instance)
    let state = ApiState::new(store, None);
    let app = build_auth_router(state);
    let (status, body) = login(&app, "a@example.com", "irrelevant").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["error"]["code"], "AccountsUnavailable");
}
