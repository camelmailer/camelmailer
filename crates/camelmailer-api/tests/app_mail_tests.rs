//! Platform email delivery (`app_mail`): the installation sends its own
//! account mail (password reset, invitation, welcome) through its own
//! sending pipeline — resolved via a server API credential and enqueued
//! over the same internal path as `POST /api/v2/server/messages`.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, build_router, ApiState};
use camelmailer_core::{
    AdminStore, AuthStore, CredentialType, DomainOwner, MemoryStore, NewCredential,
    NewOrganization, NewServer, NewUser, ServerMode,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::OnceLock;
use tower::ServiceExt;

const PASSWORD: &str = "correct-horse-battery";
const ADMIN_KEY: &str = "admin-key-000000000000";
const APP_MAIL_KEY: &str = "app-mail-key-0000000000";
const FROM_ADDRESS: &str = "no-reply@platform.example";
const FRONTEND_URL: &str = "https://mail-admin.example.com";

/// Argon2 hashing is deliberately slow; tests share one digest.
fn password_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| camelmailer_core::auth::hash_password(PASSWORD).unwrap())
}

fn app_mail_config(enabled: bool) -> camelmailer_config::Config {
    let mut config = camelmailer_config::Config::default();
    config.auth.frontend_url = Some(FRONTEND_URL.into());
    config.auth.allow_registration = true;
    config.app_mail.enabled = enabled;
    config.app_mail.server_api_key = Some(APP_MAIL_KEY.into());
    config.app_mail.from_address = Some(FROM_ADDRESS.into());
    config.app_mail.from_name = "CamelMailer".into();
    config
}

/// One organization ("Platform") with one server, a verified sending domain
/// for `FROM_ADDRESS`, and an API credential (`APP_MAIL_KEY`) — the setup an
/// operator creates to dogfood platform mail. The router merges the admin
/// and auth surfaces so every triggering endpoint is reachable.
async fn build(config: camelmailer_config::Config) -> (Router, Arc<MemoryStore>, u64) {
    let store = Arc::new(MemoryStore::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Platform".into(),
            permalink: "platform".into(),
        })
        .await
        .unwrap();
    let server = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "App Mail".into(),
            permalink: "app-mail".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    store.insert_domain(camelmailer_core::Domain {
        id: store.next_id(),
        uuid: "d".into(),
        owner: DomainOwner::Server(server.id),
        name: "platform.example".into(),
        verified: true,
        verification_token: "vtoken".into(),
        dkim_private_key: None,
    });
    store
        .create_credential_record(NewCredential {
            server_id: server.id,
            credential_type: CredentialType::Api,
            name: "app-mail".into(),
            key: Some(APP_MAIL_KEY.into()),
        })
        .await
        .unwrap();
    let state = ApiState::full(
        store.clone(),
        Some(store.clone()),
        Some(store.clone()),
        Some(ADMIN_KEY.into()),
        config,
    );
    let app = build_router(state.clone()).merge(build_auth_router(state));
    (app, store, server.id)
}

async fn create_user(store: &Arc<MemoryStore>, email: &str) -> camelmailer_core::User {
    let user = store
        .create_user(NewUser {
            email_address: email.into(),
            first_name: "Ada".into(),
            last_name: "Lovelace".into(),
            admin: false,
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
    admin_key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(key) = admin_key {
        builder = builder.header("X-Admin-API-Key", key);
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

/// Raw MIME as a string with quoted-printable soft line breaks and `=3D`
/// escapes undone, so links can be asserted/extracted verbatim.
fn decoded_raw(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw)
        .replace("=\r\n", "")
        .replace("=\n", "")
        .replace("=3D", "=")
}

/// The token following `marker` (base64url alphabet).
fn extract_token(decoded: &str, marker: &str) -> String {
    let start = decoded.find(marker).expect("link marker in mail body") + marker.len();
    decoded[start..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

// ---------------------------------------------------------------- disabled

#[tokio::test]
async fn disabled_app_mail_enqueues_nothing_and_changes_no_behaviour() {
    let (app, store, server_id) = build(app_mail_config(false)).await;
    create_user(&store, "ada@example.com").await;

    // password reset: same response, no mail
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!({ "reset_requested": true }));

    // registration: same response, no mail
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email_address": "grace@example.com",
            "first_name": "Grace",
            "last_name": "Hopper",
            "password": PASSWORD,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // invitation: token + url still returned, no mail
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/platform/invitations",
        Some(ADMIN_KEY),
        Some(json!({ "email_address": "invitee@example.com", "role": "member" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["data"]["invitation"]["invite_token"].is_string());
    assert!(body["data"]["invitation"]["invite_url"].is_string());

    assert!(store.messages_for(server_id).is_empty());
}

// ------------------------------------------------------------ password reset

#[tokio::test]
async fn password_reset_emails_the_link_and_keeps_the_token_out_of_the_response() {
    let (app, store, server_id) = build(app_mail_config(true)).await;
    create_user(&store, "ada@example.com").await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // the response carries no token or link — only the acknowledgement
    assert_eq!(body["data"], json!({ "reset_requested": true }));

    // exactly one mail, to the user, from the configured sender
    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].rcpt_to, "ada@example.com");
    assert_eq!(stored[0].mail_from, FROM_ADDRESS);
    assert_eq!(stored[0].scope, "outgoing");

    let raw = decoded_raw(&stored[0].raw_message);
    assert!(raw.contains("Subject: Reset your CamelMailer password"));
    assert!(raw.contains("CamelMailer"), "From display name");
    assert!(raw.contains(&format!("{FRONTEND_URL}/reset-password?token=")));

    // the emailed link actually completes the reset (end to end)
    let token = extract_token(&raw, "/reset-password?token=");
    assert_eq!(token.len(), 43);
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset/complete",
        None,
        Some(json!({ "token": token, "new_password": "brand-new-pass-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["password_reset"], true);
}

#[tokio::test]
async fn reset_for_an_unknown_address_sends_no_mail_and_responds_identically() {
    let (app, store, server_id) = build(app_mail_config(true)).await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset",
        None,
        Some(json!({ "email_address": "ghost@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!({ "reset_requested": true }));
    assert!(store.messages_for(server_id).is_empty());
}

// --------------------------------------------------------------- invitation

#[tokio::test]
async fn invitation_emails_the_accept_link_and_still_returns_the_token() {
    let (app, store, server_id) = build(app_mail_config(true)).await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/platform/invitations",
        Some(ADMIN_KEY),
        Some(json!({ "email_address": "invitee@example.com", "role": "member" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // the inviting admin still sees the token (returned exactly once)
    let invite_token = body["data"]["invitation"]["invite_token"]
        .as_str()
        .unwrap()
        .to_string();

    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].rcpt_to, "invitee@example.com");
    assert_eq!(stored[0].mail_from, FROM_ADDRESS);

    let raw = decoded_raw(&stored[0].raw_message);
    assert!(raw.contains("Subject: You have been invited to Platform on CamelMailer"));
    assert!(raw.contains(&format!("{FRONTEND_URL}/invitations/accept?token=")));
    // the emailed link carries the same token the admin was shown
    assert_eq!(
        extract_token(&raw, "/invitations/accept?token="),
        invite_token
    );
}

// ------------------------------------------------------------------ welcome

#[tokio::test]
async fn registration_sends_a_welcome_mail_without_any_token() {
    let (app, store, server_id) = build(app_mail_config(true)).await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email_address": "grace@example.com",
            "first_name": "Grace",
            "last_name": "Hopper",
            "password": PASSWORD,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["data"]["session_token"].is_string());

    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].rcpt_to, "grace@example.com");
    assert_eq!(stored[0].mail_from, FROM_ADDRESS);

    let raw = decoded_raw(&stored[0].raw_message);
    assert!(raw.contains("Subject: Welcome to CamelMailer"));
    assert!(raw.contains("Hello Grace"));
    assert!(raw.contains(FRONTEND_URL), "sign-in link");
    assert!(!raw.contains("?token="), "welcome mail carries no token");
}

// ---------------------------------------------------------------- failures

#[tokio::test]
async fn an_invalid_server_api_key_never_fails_the_request() {
    let mut config = app_mail_config(true);
    config.app_mail.server_api_key = Some("wrong-key".into());
    let (app, store, server_id) = build(config).await;
    create_user(&store, "ada@example.com").await;

    // password reset still succeeds (and falls back to logging the link)
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!({ "reset_requested": true }));

    // registration still succeeds
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/auth/register",
        None,
        Some(json!({
            "email_address": "grace@example.com",
            "first_name": "Grace",
            "last_name": "Hopper",
            "password": PASSWORD,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // invitation still succeeds with the token in the response
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/platform/invitations",
        Some(ADMIN_KEY),
        Some(json!({ "email_address": "invitee@example.com", "role": "member" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["data"]["invitation"]["invite_token"].is_string());

    assert!(store.messages_for(server_id).is_empty());
}

#[tokio::test]
async fn a_missing_frontend_url_skips_the_mail_but_not_the_request() {
    let mut config = app_mail_config(true);
    config.auth.frontend_url = None;
    let (app, store, server_id) = build(config).await;
    create_user(&store, "ada@example.com").await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/password-reset",
        None,
        Some(json!({ "email_address": "ada@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"], json!({ "reset_requested": true }));
    // no link can be built, so no reset mail is sent
    assert!(store.messages_for(server_id).is_empty());
}
