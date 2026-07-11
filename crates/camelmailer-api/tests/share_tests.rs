//! Message share links: creation via the per-server API and the public,
//! unauthenticated share endpoint (`/api/v2/share/messages/{token}`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_server_router, build_share_router, ApiState};
use camelmailer_core::mime::{Address, BuildParams};
use camelmailer_core::{
    AdminStore, CredentialType, MemoryStore, MessageScope, NewCredential, NewOrganization,
    NewServer, QueuedMessage, ServerMode, StaticDnsResolver,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const FRONTEND: &str = "https://app.camelmailer.example";

struct Setup {
    app: Router,
    store: Arc<MemoryStore>,
    token_a: String,
    token_b: String,
    server_a_id: camelmailer_core::Id,
    message_id: i64,
}

/// Two servers in one org; server A owns one stored message with a
/// delivery, an open and a click.
async fn build() -> Setup {
    let store = Arc::new(MemoryStore::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Org".into(),
            permalink: "org".into(),
        })
        .await
        .unwrap();
    let server_a = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "Alpha".into(),
            permalink: "alpha".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let server_b = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "Beta".into(),
            permalink: "beta".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let token_a = "tok-alpha-000000000000".to_string();
    let token_b = "tok-beta-0000000000000".to_string();
    for (server_id, key) in [(server_a.id, &token_a), (server_b.id, &token_b)] {
        store
            .create_credential_record(NewCredential {
                server_id,
                credential_type: CredentialType::Api,
                name: "api".into(),
                key: Some(key.clone()),
            })
            .await
            .unwrap();
    }

    let raw = camelmailer_core::mime::build_message(&BuildParams {
        from: Address::new("hello@example.com"),
        to: vec![Address::new("rcpt@dest.example")],
        subject: "Your invoice".into(),
        html_body: Some("<p>Hello</p>".into()),
        text_body: Some("Hello".into()),
        ..Default::default()
    });
    let sent = store.insert_message_record(QueuedMessage {
        server_id: server_a.id,
        rcpt_to: "rcpt@dest.example".into(),
        mail_from: "hello@example.com".into(),
        raw_message: raw,
        received_with_ssl: false,
        scope: MessageScope::Outgoing,
        bounce: false,
        domain_id: None,
        credential_id: None,
        route_id: None,
        tag: None,
        metadata: None,
        stream_id: None,
    });
    store.insert_delivery_record(
        sent.id,
        camelmailer_core::DeliveryRecord {
            id: 1,
            status: "Sent".into(),
            details: Some("250 ok".into()),
            output: None,
            sent_with_ssl: true,
            created_at: chrono::Utc::now(),
        },
    );
    store.insert_open_record(
        sent.id,
        camelmailer_core::ActivityEvent {
            ip_address: Some("192.0.2.1".into()),
            user_agent: Some("test-agent".into()),
            url: None,
            created_at: chrono::Utc::now(),
        },
    );
    store.insert_click_record(
        sent.id,
        camelmailer_core::ActivityEvent {
            ip_address: Some("192.0.2.2".into()),
            user_agent: Some("test-agent".into()),
            url: Some("https://example.com/x".into()),
            created_at: chrono::Utc::now(),
        },
    );

    let mut config = camelmailer_config::Config::default();
    config.auth.frontend_url = Some(format!("{FRONTEND}/"));
    let state = ApiState::full_with_resolver(
        store.clone(),
        Some(store.clone()),
        None,
        None,
        config,
        Arc::new(StaticDnsResolver::new()),
    );
    let app = build_server_router(state.clone()).merge(build_share_router(state));
    Setup {
        app,
        store,
        token_a,
        token_b,
        server_a_id: server_a.id,
        message_id: sent.id,
    }
}

async fn post_share(
    app: &Router,
    token: &str,
    message_id: i64,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/api/v2/server/messages/{message_id}/share"))
        .header("X-Server-API-Key", token);
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get_public(app: &Router, path: &str) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(path)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn hours_from_now(rfc3339: &str) -> f64 {
    let at = chrono::DateTime::parse_from_rfc3339(rfc3339).unwrap();
    (at.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_minutes() as f64 / 60.0
}

#[tokio::test]
async fn creating_a_share_returns_a_frontend_url_with_default_expiry() {
    let setup = build().await;
    let (status, body) = post_share(&setup.app, &setup.token_a, setup.message_id, None).await;
    assert_eq!(status, StatusCode::CREATED);
    let url = body["data"]["url"].as_str().unwrap();
    assert!(
        url.starts_with(&format!("{FRONTEND}/share/m/")),
        "unexpected url {url}"
    );
    // default expiry is 48 hours
    let hours = hours_from_now(body["data"]["expires_at"].as_str().unwrap());
    assert!((47.0..=48.5).contains(&hours), "expiry {hours}h");

    // the token is never stored — only its hash resolves the share
    let token = url.rsplit('/').next().unwrap();
    assert!(setup.store.find_message_share(token).is_none());
    assert!(setup
        .store
        .find_message_share(&camelmailer_core::auth::hash_token(token))
        .is_some());
}

#[tokio::test]
async fn custom_expiry_is_respected_and_bounded() {
    let setup = build().await;
    let (status, body) = post_share(
        &setup.app,
        &setup.token_a,
        setup.message_id,
        Some(json!({ "expires_in_hours": 24 })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let hours = hours_from_now(body["data"]["expires_at"].as_str().unwrap());
    assert!((23.0..=24.5).contains(&hours), "expiry {hours}h");

    for invalid in [0, -5, 169] {
        let (status, body) = post_share(
            &setup.app,
            &setup.token_a,
            setup.message_id,
            Some(json!({ "expires_in_hours": invalid })),
        )
        .await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body["error"]["code"], "ValidationError");
    }
}

#[tokio::test]
async fn a_foreign_servers_message_cannot_be_shared() {
    let setup = build().await;
    let (status, body) = post_share(&setup.app, &setup.token_b, setup.message_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
}

#[tokio::test]
async fn the_public_endpoint_serves_the_shared_message_without_auth() {
    let setup = build().await;
    let (_, created) = post_share(&setup.app, &setup.token_a, setup.message_id, None).await;
    let token = created["data"]["url"]
        .as_str()
        .unwrap()
        .rsplit('/')
        .next()
        .unwrap()
        .to_string();

    let (status, body) = get_public(&setup.app, &format!("/api/v2/share/messages/{token}")).await;
    assert_eq!(status, StatusCode::OK);
    let data = &body["data"];
    assert_eq!(data["message"]["id"], setup.message_id);
    assert_eq!(data["message"]["subject"], "Your invoice");
    assert_eq!(data["deliveries"][0]["status"], "Sent");
    assert_eq!(data["opens"][0]["ip_address"], "192.0.2.1");
    assert_eq!(data["clicks"][0]["url"], "https://example.com/x");
    assert_eq!(data["html_body"], "<p>Hello</p>");
    assert_eq!(data["text_body"], "Hello");
    assert!(data["expires_at"].is_string());
}

#[tokio::test]
async fn unknown_tokens_are_not_found() {
    let setup = build().await;
    let (status, body) = get_public(&setup.app, "/api/v2/share/messages/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
}

#[tokio::test]
async fn expired_links_answer_with_the_stable_expiry_code() {
    let setup = build().await;
    // seed a share that expired an hour ago
    let token = "expired-share-token";
    setup
        .store
        .insert_message_share(camelmailer_core::NewMessageShare {
            server_id: setup.server_a_id,
            message_id: setup.message_id,
            token_hash: camelmailer_core::auth::hash_token(token),
            expires_at: chrono::Utc::now() - chrono::Duration::hours(1),
        });
    let (status, body) = get_public(&setup.app, &format!("/api/v2/share/messages/{token}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "ShareLinkExpired");
}
