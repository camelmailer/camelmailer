//! Per-address sender signatures, end to end: add an address over the
//! management API, confirm it over the public auth endpoint, and watch the
//! From authorization of the HTTP send path flip from rejected to allowed.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, build_router, build_server_router, ApiState};
use camelmailer_core::{
    AdminStore, CredentialType, DomainOwner, MemoryStore, NewCredential, NewOrganization,
    NewServer, ServerMode,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const ADMIN_KEY: &str = "admin-key-000000000000";
const SERVER_KEY: &str = "server-key-00000000000";
const APP_MAIL_KEY: &str = "app-mail-key-000000000";
const BASE: &str = "/api/v2/admin/organizations/acme/servers/mail";

/// One org ("acme") with one mail server ("mail") and an API credential.
/// The admin, auth and per-server routers are merged so the whole flow can
/// be exercised against a single app.
async fn build(config: camelmailer_config::Config) -> (Router, Arc<MemoryStore>, u64) {
    let store = Arc::new(MemoryStore::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Acme".into(),
            permalink: "acme".into(),
        })
        .await
        .unwrap();
    let server = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "Mail".into(),
            permalink: "mail".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    store
        .create_credential_record(NewCredential {
            server_id: server.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(SERVER_KEY.into()),
        })
        .await
        .unwrap();
    let state = ApiState::full(
        store.clone(),
        Some(store.clone()),
        None,
        Some(ADMIN_KEY.into()),
        config,
    );
    let app = build_router(state.clone())
        .merge(build_auth_router(state.clone()))
        .merge(build_server_router(state));
    (app, store, server.id)
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
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

fn send_body(from: &str) -> Value {
    json!({
        "from": from,
        "to": ["someone@dest.example"],
        "subject": "Hello",
        "text_body": "Hi.",
    })
}

async fn send_from(app: &Router, from: &str) -> (StatusCode, Value) {
    request(
        app,
        "POST",
        "/api/v2/server/messages",
        &[("X-Server-API-Key", SERVER_KEY)],
        Some(send_body(from)),
    )
    .await
}

#[tokio::test]
async fn the_full_flow_flips_send_authorization() {
    let mut config = camelmailer_config::Config::default();
    config.auth.frontend_url = Some("https://mail-admin.example.com".into());
    let (app, _store, _server_id) = build(config).await;

    // before: the address does not authenticate a send
    let (status, body) = send_from(&app, "solo@external.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "ValidationError");

    // add the address; app_mail is disabled → the token comes back once
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/sender_addresses"),
        &[("X-Admin-API-Key", ADMIN_KEY)],
        Some(json!({ "email": "solo@external.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let token = body["data"]["verification_token"]
        .as_str()
        .unwrap()
        .to_string();
    let id = body["data"]["sender_address"]["id"].as_u64().unwrap();

    // pending: still rejected
    let (status, _) = send_from(&app, "solo@external.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // a wrong token does not confirm
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/sender-addresses/confirm",
        &[],
        Some(json!({ "token": "wrong" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "InvalidToken");

    // confirm (no auth needed — the token is the secret)
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/auth/sender-addresses/confirm",
        &[],
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["confirmed"], true);
    assert_eq!(body["data"]["email_address"], "solo@external.example");

    // the token is single-use
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/auth/sender-addresses/confirm",
        &[],
        Some(json!({ "token": token })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // after: the exact address now sends (case-insensitively)…
    let (status, body) = send_from(&app, "solo@external.example").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let (status, _) = send_from(&app, "Solo@External.example").await;
    assert_eq!(status, StatusCode::CREATED);

    // …the list shows it confirmed…
    let (status, body) = request(
        &app,
        "GET",
        &format!("{BASE}/sender_addresses"),
        &[("X-Admin-API-Key", ADMIN_KEY)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["sender_addresses"][0]["status"], "confirmed");

    // …other addresses of the same domain stay unauthorized (exact match)
    let (status, _) = send_from(&app, "other@external.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // deleting the address revokes the authorization
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/sender_addresses/{id}"),
        &[("X-Admin-API-Key", ADMIN_KEY)],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send_from(&app, "solo@external.example").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn verified_domains_still_authorize_without_a_sender_address() {
    let (app, store, server_id) = build(camelmailer_config::Config::default()).await;
    store.insert_domain(camelmailer_core::Domain {
        id: store.next_id(),
        uuid: "d".into(),
        owner: DomainOwner::Server(server_id),
        name: "acme.example".into(),
        verified: true,
        check_dmarc: true,
        dkim_private_key: None,
        verification_token: String::new(),
    });
    let (status, body) = send_from(&app, "anyone@acme.example").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

/// With app_mail enabled the confirmation link is emailed through the
/// installation's own pipeline and the token stays out of the response.
#[tokio::test]
async fn with_app_mail_enabled_the_token_is_mailed_not_returned() {
    let mut config = camelmailer_config::Config::default();
    config.auth.frontend_url = Some("https://mail-admin.example.com".into());
    config.app_mail.enabled = true;
    config.app_mail.server_api_key = Some(APP_MAIL_KEY.into());
    config.app_mail.from_address = Some("no-reply@platform.example".into());
    let (app, store, _server_id) = build(config).await;

    // the platform's own mail server (the app_mail credential target)
    let org = store
        .create_organization(NewOrganization {
            name: "Platform".into(),
            permalink: "platform".into(),
        })
        .await
        .unwrap();
    let platform = store
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
        uuid: "pd".into(),
        owner: DomainOwner::Server(platform.id),
        name: "platform.example".into(),
        verified: true,
        check_dmarc: true,
        dkim_private_key: None,
        verification_token: String::new(),
    });
    store
        .create_credential_record(NewCredential {
            server_id: platform.id,
            credential_type: CredentialType::Api,
            name: "app-mail".into(),
            key: Some(APP_MAIL_KEY.into()),
        })
        .await
        .unwrap();

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/sender_addresses"),
        &[("X-Admin-API-Key", ADMIN_KEY)],
        Some(json!({ "email": "solo@external.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    // no token in the response — it went out by mail
    assert!(body["data"]["verification_token"].is_null());

    // the confirmation mail was enqueued on the platform server, to
    // exactly the added address, carrying the confirm link
    let messages = store.messages_for(platform.id);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].rcpt_to, "solo@external.example");
    let raw = String::from_utf8_lossy(&messages[0].raw_message).to_string();
    assert!(
        raw.contains("/sender-addresses/confirm?token="),
        "confirmation mail must carry the confirm link"
    );
}
