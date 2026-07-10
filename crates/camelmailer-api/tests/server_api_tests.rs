//! Per-server API (`/api/v2/server`) tests: server-token auth + scoping.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_server_router, ApiState};
use camelmailer_core::{
    AdminStore, CredentialType, MemoryStore, NewCredential, NewOrganization, NewServer, ServerMode,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

/// Build a router with two organizations, each with one server and one API
/// token, so scoping can be exercised. Returns (router, token_a, token_b,
/// server_a_permalink, server_b_permalink).
async fn build() -> (Router, String, String) {
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
    store
        .create_credential_record(NewCredential {
            server_id: server_a.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(token_a.clone()),
        })
        .await
        .unwrap();
    store
        .create_credential_record(NewCredential {
            server_id: server_b.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(token_b.clone()),
        })
        .await
        .unwrap();

    let state = ApiState::with_server_store(store.clone(), store, None);
    (build_server_router(state), token_a, token_b)
}

async fn request(app: &Router, path: &str, token: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method("GET").uri(path);
    if let Some(token) = token {
        builder = builder.header("X-Server-API-Key", token);
    }
    let response = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn missing_token_is_unauthorized() {
    let (app, _, _) = build().await;
    let (status, body) = request(&app, "/api/v2/server/ping", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], "Missing X-Server-API-Key header");
    assert!(body["time"].is_number());
}

#[tokio::test]
async fn invalid_token_is_unauthorized() {
    let (app, _, _) = build().await;
    let (status, body) = request(&app, "/api/v2/server/ping", Some("nope")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], "Invalid server API token");
}

#[tokio::test]
async fn valid_token_resolves_the_right_server() {
    let (app, token_a, _) = build().await;
    let (status, body) = request(&app, "/api/v2/server/ping", Some(&token_a)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
    assert_eq!(body["data"]["pong"], true);
    assert_eq!(body["data"]["server"], "alpha");

    // GET /api/v2/server returns the scoped server
    let (status, body) = request(&app, "/api/v2/server", Some(&token_a)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["server"]["permalink"], "alpha");
    assert_eq!(body["data"]["server"]["name"], "Alpha");
}

#[tokio::test]
async fn tokens_are_scoped_to_their_own_server() {
    let (app, token_a, token_b) = build().await;
    let (_, body_a) = request(&app, "/api/v2/server", Some(&token_a)).await;
    let (_, body_b) = request(&app, "/api/v2/server", Some(&token_b)).await;
    // token A only ever sees alpha, token B only ever sees beta
    assert_eq!(body_a["data"]["server"]["permalink"], "alpha");
    assert_eq!(body_b["data"]["server"]["permalink"], "beta");
    assert_ne!(
        body_a["data"]["server"]["id"],
        body_b["data"]["server"]["id"]
    );
}

#[tokio::test]
async fn suspended_server_token_is_rejected() {
    let store = Arc::new(MemoryStore::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Org".into(),
            permalink: "org".into(),
        })
        .await
        .unwrap();
    let mut server = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "S".into(),
            permalink: "s".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let token = "tok-suspended-00000000".to_string();
    store
        .create_credential_record(NewCredential {
            server_id: server.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(token.clone()),
        })
        .await
        .unwrap();
    server.suspended = true;
    store.update_server(server).await.unwrap();

    let state = ApiState::with_server_store(store.clone(), store, None);
    let app = build_server_router(state);
    let (status, body) = request(&app, "/api/v2/server/ping", Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], "This server has been suspended");
}

#[tokio::test]
async fn held_api_credential_does_not_authenticate() {
    let store = Arc::new(MemoryStore::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Org".into(),
            permalink: "org".into(),
        })
        .await
        .unwrap();
    let server = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "S".into(),
            permalink: "s".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let token = "tok-held-0000000000000".to_string();
    let mut credential = store
        .create_credential_record(NewCredential {
            server_id: server.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(token.clone()),
        })
        .await
        .unwrap();
    credential.hold = true;
    store.update_credential(credential).await.unwrap();

    let state = ApiState::with_server_store(store.clone(), store, None);
    let app = build_server_router(state);
    let (status, _) = request(&app, "/api/v2/server/ping", Some(&token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ------------------------------------------------------ P2: HTTP send

use camelmailer_core::{DomainOwner, MemoryStore as MS};

async fn build_with_verified_domain() -> (Router, String, Arc<MS>, u64) {
    let store = Arc::new(MS::new());
    let org = store
        .create_organization(NewOrganization { name: "Org".into(), permalink: "org".into() })
        .await
        .unwrap();
    let server = store
        .create_server(NewServer {
            organization_id: org.id,
            name: "S".into(),
            permalink: "s".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    // a verified server-owned domain
    store.insert_domain(camelmailer_core::Domain {
        id: store.next_id(),
        uuid: "d".into(),
        owner: DomainOwner::Server(server.id),
        name: "org.example".into(),
        verified: true,
    });
    let token = "send-token-0000000000".to_string();
    store
        .create_credential_record(NewCredential {
            server_id: server.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(token.clone()),
        })
        .await
        .unwrap();
    let state = ApiState::with_server_store(store.clone(), store.clone(), None);
    (build_server_router(state), token, store, server.id)
}

async fn post_json(app: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("X-Server-API-Key", token)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

#[tokio::test]
async fn send_stores_one_message_per_recipient() {
    let (app, token, store, server_id) = build_with_verified_domain().await;
    let (status, body) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@org.example",
            "to": ["a@dest.example", {"email": "b@dest.example", "name": "B"}],
            "cc": ["c@dest.example"],
            "subject": "Hello",
            "html_body": "<p>Hi</p>",
            "text_body": "Hi",
            "tag": "welcome",
            "metadata": { "campaign": "spring" }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let recipients = body["data"]["recipients"].as_array().unwrap();
    assert_eq!(recipients.len(), 3);
    assert!(body["data"]["message_id"].is_number());
    assert_eq!(recipients[0]["status"], "queued");

    // three stored messages, all outgoing, tagged
    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 3);
    assert!(stored.iter().all(|m| m.scope == "outgoing"));
    assert!(stored.iter().all(|m| m.tag.as_deref() == Some("welcome")));
    let raw = String::from_utf8_lossy(&stored[0].raw_message);
    assert!(raw.contains("Subject: Hello"));
}

#[tokio::test]
async fn send_from_an_unverified_domain_is_rejected() {
    let (app, token, _, _) = build_with_verified_domain().await;
    let (status, body) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@not-mine.example",
            "to": ["a@dest.example"],
            "subject": "Hi",
            "text_body": "x"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
}

#[tokio::test]
async fn send_without_from_or_recipients_is_a_parameter_error() {
    let (app, token, _, _) = build_with_verified_domain().await;
    let (status, _) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({ "to": ["a@dest.example"], "subject": "x", "text_body": "y" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({ "from": "news@org.example", "subject": "x", "text_body": "y" }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn batch_send_returns_per_message_results() {
    let (app, token, store, server_id) = build_with_verified_domain().await;
    let (status, body) = post_json(
        &app,
        "/api/v2/server/messages/batch",
        &token,
        json!([
            { "from": "news@org.example", "to": ["a@dest.example"], "subject": "1", "text_body": "x" },
            { "from": "news@bad.example", "to": ["b@dest.example"], "subject": "2", "text_body": "y" }
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let results = body["data"]["messages"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["status"], "success");
    assert_eq!(results[1]["status"], "error");
    // only the first (valid) message was stored
    assert_eq!(store.messages_for(server_id).len(), 1);
}
