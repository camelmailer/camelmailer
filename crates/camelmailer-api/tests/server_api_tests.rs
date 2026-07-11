//! Per-server API (`/api/v2/server`) tests: server-token auth + scoping.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
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
    // a verified server-owned domain
    store.insert_domain(camelmailer_core::Domain {
        id: store.next_id(),
        uuid: "d".into(),
        owner: DomainOwner::Server(server.id),
        name: "org.example".into(),
        verified: true,
        verification_token: "vtoken".into(),
        dkim_private_key: None,
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
    json_request(app, "POST", path, token, body).await
}

async fn patch_json(app: &Router, path: &str, token: &str, body: Value) -> (StatusCode, Value) {
    json_request(app, "PATCH", path, token, body).await
}

async fn json_request(
    app: &Router,
    method: &str,
    path: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
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
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
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

// ------------------------------------------------ P3: message read APIs

/// Two servers, each with a verified domain + API token, so cross-tenant
/// isolation can be exercised. Returns (router, token_a, token_b, store).
async fn build_two_with_domains() -> (Router, String, String, Arc<MS>) {
    let store = Arc::new(MS::new());
    let org = store
        .create_organization(NewOrganization {
            name: "Org".into(),
            permalink: "org".into(),
        })
        .await
        .unwrap();
    let mut tokens = Vec::new();
    for (name, domain) in [("Alpha", "alpha.example"), ("Beta", "beta.example")] {
        let server = store
            .create_server(NewServer {
                organization_id: org.id,
                name: name.into(),
                permalink: name.to_lowercase(),
                mode: ServerMode::Live,
            })
            .await
            .unwrap();
        store.insert_domain(camelmailer_core::Domain {
            id: store.next_id(),
            uuid: format!("d-{name}"),
            owner: DomainOwner::Server(server.id),
            name: domain.into(),
            verified: true,
            verification_token: "vtoken".into(),
            dkim_private_key: None,
        });
        let token = format!("tok-{}-000000000000", name.to_lowercase());
        store
            .create_credential_record(NewCredential {
                server_id: server.id,
                credential_type: CredentialType::Api,
                name: "api".into(),
                key: Some(token.clone()),
            })
            .await
            .unwrap();
        tokens.push(token);
    }
    let state = ApiState::with_server_store(store.clone(), store.clone(), None);
    (
        build_server_router(state),
        tokens[0].clone(),
        tokens[1].clone(),
        store,
    )
}

/// Send one message and return the stored message id.
async fn send_one(
    app: &Router,
    token: &str,
    from: &str,
    to: &str,
    subject: &str,
    tag: &str,
) -> i64 {
    let (status, body) = post_json(
        app,
        "/api/v2/server/messages",
        token,
        json!({ "from": from, "to": [to], "subject": subject, "text_body": "x", "tag": tag }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["data"]["message_id"].as_i64().unwrap()
}

#[tokio::test]
async fn messages_index_lists_filters_and_paginates() {
    let (app, token, _, _) = build_two_with_domains().await;
    send_one(
        &app,
        &token,
        "news@alpha.example",
        "one@dest.example",
        "First",
        "welcome",
    )
    .await;
    send_one(
        &app,
        &token,
        "news@alpha.example",
        "two@dest.example",
        "Second",
        "promo",
    )
    .await;

    // full list, newest first
    let (status, body) = request(&app, "/api/v2/server/messages", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let messages = body["data"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["subject"], "Second");
    assert_eq!(body["data"]["pagination"]["total"], 2);

    // filter by tag
    let (_, body) = request(&app, "/api/v2/server/messages?tag=promo", Some(&token)).await;
    let messages = body["data"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["subject"], "Second");

    // filter by recipient substring
    let (_, body) = request(&app, "/api/v2/server/messages?query=one@dest", Some(&token)).await;
    assert_eq!(body["data"]["messages"].as_array().unwrap().len(), 1);

    // pagination
    let (_, body) = request(
        &app,
        "/api/v2/server/messages?per_page=1&page=2",
        Some(&token),
    )
    .await;
    assert_eq!(body["data"]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["pagination"]["total_pages"], 2);
}

#[tokio::test]
async fn message_show_includes_deliveries_opens_and_clicks() {
    let (app, token, _, store) = build_two_with_domains().await;
    let id = send_one(
        &app,
        &token,
        "news@alpha.example",
        "one@dest.example",
        "Hi",
        "t",
    )
    .await;

    store.insert_delivery_record(
        id,
        camelmailer_core::DeliveryRecord {
            id: 1,
            status: "Sent".into(),
            details: Some("250 OK".into()),
            output: None,
            sent_with_ssl: true,
            created_at: chrono::Utc::now(),
        },
    );
    store.insert_open_record(
        id,
        camelmailer_core::ActivityEvent {
            ip_address: Some("1.2.3.4".into()),
            user_agent: Some("Mail".into()),
            url: None,
            created_at: chrono::Utc::now(),
        },
    );
    store.insert_click_record(
        id,
        camelmailer_core::ActivityEvent {
            ip_address: Some("1.2.3.4".into()),
            user_agent: Some("Mail".into()),
            url: Some("https://example.com".into()),
            created_at: chrono::Utc::now(),
        },
    );

    // show carries the message + its deliveries
    let (status, body) =
        request(&app, &format!("/api/v2/server/messages/{id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["message"]["subject"], "Hi");
    assert_eq!(body["data"]["message"]["status"], "Pending");
    assert_eq!(body["data"]["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["deliveries"][0]["status"], "Sent");

    // dedicated activity endpoints
    let (_, body) = request(
        &app,
        &format!("/api/v2/server/messages/{id}/deliveries"),
        Some(&token),
    )
    .await;
    assert_eq!(body["data"]["deliveries"].as_array().unwrap().len(), 1);
    let (_, body) = request(
        &app,
        &format!("/api/v2/server/messages/{id}/opens"),
        Some(&token),
    )
    .await;
    assert_eq!(body["data"]["opens"].as_array().unwrap().len(), 1);
    let (_, body) = request(
        &app,
        &format!("/api/v2/server/messages/{id}/clicks"),
        Some(&token),
    )
    .await;
    assert_eq!(body["data"]["clicks"][0]["url"], "https://example.com");
}

#[tokio::test]
async fn unknown_message_is_not_found() {
    let (app, token, _, _) = build_two_with_domains().await;
    let (status, body) = request(&app, "/api/v2/server/messages/999999", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
}

#[tokio::test]
async fn message_raw_returns_base64_and_respects_privacy_mode() {
    let (app, token, _, store) = build_two_with_domains().await;
    let id = send_one(
        &app,
        &token,
        "news@alpha.example",
        "one@dest.example",
        "Hi",
        "t",
    )
    .await;

    let (status, body) = request(
        &app,
        &format!("/api/v2/server/messages/{id}/raw"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw_b64 = body["data"]["raw_message"].as_str().unwrap();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(raw_b64)
        .unwrap();
    assert!(String::from_utf8_lossy(&decoded).contains("Subject: Hi"));

    // enabling privacy mode withholds the raw content
    let mut server = store.server_for_api_token(&token).await.unwrap().unwrap();
    server.privacy_mode = true;
    store.update_server(server).await.unwrap();
    let (status, body) = request(
        &app,
        &format!("/api/v2/server/messages/{id}/raw"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotAvailable");
}

#[tokio::test]
async fn a_server_token_cannot_read_another_servers_message() {
    let (app, token_a, token_b, _) = build_two_with_domains().await;
    let id = send_one(
        &app,
        &token_a,
        "news@alpha.example",
        "one@dest.example",
        "Secret",
        "t",
    )
    .await;

    // token B's list never includes server A's message
    let (_, body) = request(&app, "/api/v2/server/messages", Some(&token_b)).await;
    assert_eq!(body["data"]["messages"].as_array().unwrap().len(), 0);

    // and every per-message endpoint is a 404 for token B
    for suffix in ["", "/deliveries", "/opens", "/clicks", "/raw"] {
        let (status, _) = request(
            &app,
            &format!("/api/v2/server/messages/{id}{suffix}"),
            Some(&token_b),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "endpoint {suffix} leaked");
    }
}

// ------------------------------------------- P4: stats + bounces

#[tokio::test]
async fn stats_aggregate_status_and_engagement() {
    let (app, token, _, store) = build_two_with_domains().await;
    let a = send_one(
        &app,
        &token,
        "news@alpha.example",
        "one@dest.example",
        "A",
        "t",
    )
    .await;
    let b = send_one(
        &app,
        &token,
        "news@alpha.example",
        "two@dest.example",
        "B",
        "t",
    )
    .await;

    // mark one Sent (with an open + two clicks), leave the other Pending
    store.set_message_status(a, "Sent");
    store.insert_open_record(a, open_event());
    store.insert_click_record(a, click_event());
    store.insert_click_record(a, click_event());

    let (status, body) = request(&app, "/api/v2/server/stats", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let s = &body["data"]["stats"];
    assert_eq!(s["total"], 2);
    assert_eq!(s["outgoing"], 2);
    assert_eq!(s["sent"], 1);
    assert_eq!(s["pending"], 1);
    assert_eq!(s["opens"], 1);
    assert_eq!(s["unique_opens"], 1);
    assert_eq!(s["clicks"], 2);
    assert_eq!(s["unique_clicks"], 1);

    // only b is still Pending → the outbound queue snapshot shows one
    let (_, body) = request(&app, "/api/v2/server/stats/deliveries", Some(&token)).await;
    assert_eq!(body["data"]["queued"], 1);
    assert_eq!(body["data"]["domains"][0]["queued"], 1);
    let _ = b;
}

fn open_event() -> camelmailer_core::ActivityEvent {
    camelmailer_core::ActivityEvent {
        ip_address: Some("1.2.3.4".into()),
        user_agent: Some("Mail".into()),
        url: None,
        created_at: chrono::Utc::now(),
    }
}

fn click_event() -> camelmailer_core::ActivityEvent {
    camelmailer_core::ActivityEvent {
        ip_address: Some("1.2.3.4".into()),
        user_agent: Some("Mail".into()),
        url: Some("https://example.com".into()),
        created_at: chrono::Utc::now(),
    }
}

#[tokio::test]
async fn bounces_list_and_show_only_expose_bounces() {
    let (app, token, _, store) = build_two_with_domains().await;
    let normal = send_one(
        &app,
        &token,
        "news@alpha.example",
        "one@dest.example",
        "OK",
        "t",
    )
    .await;
    let bounced = send_one(
        &app,
        &token,
        "news@alpha.example",
        "two@dest.example",
        "Bad",
        "t",
    )
    .await;
    store.set_message_status(bounced, "Bounced");

    // list contains only the bounced message
    let (status, body) = request(&app, "/api/v2/server/bounces", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let bounces = body["data"]["bounces"].as_array().unwrap();
    assert_eq!(bounces.len(), 1);
    assert_eq!(bounces[0]["subject"], "Bad");

    // show works for the bounce, 404 for a non-bounce
    let (status, _) = request(
        &app,
        &format!("/api/v2/server/bounces/{bounced}"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        &format!("/api/v2/server/bounces/{normal}"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_and_bounces_are_tenant_scoped() {
    let (app, token_a, token_b, store) = build_two_with_domains().await;
    let id = send_one(
        &app,
        &token_a,
        "news@alpha.example",
        "one@dest.example",
        "A",
        "t",
    )
    .await;
    store.set_message_status(id, "Bounced");

    // token B sees an empty stat line and no bounces
    let (_, body) = request(&app, "/api/v2/server/stats", Some(&token_b)).await;
    assert_eq!(body["data"]["stats"]["total"], 0);
    let (_, body) = request(&app, "/api/v2/server/bounces", Some(&token_b)).await;
    assert_eq!(body["data"]["bounces"].as_array().unwrap().len(), 0);
    let (status, _) = request(
        &app,
        &format!("/api/v2/server/bounces/{id}"),
        Some(&token_b),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ------------------------------------------- P5: message streams

#[tokio::test]
async fn every_server_has_a_default_stream() {
    let (app, token, _, _) = build_two_with_domains().await;
    let (status, body) = request(&app, "/api/v2/server/streams", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let streams = body["data"]["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0]["permalink"], "outbound");
    assert_eq!(streams[0]["stream_type"], "transactional");

    // the server exposes its default_stream_id
    let (_, body) = request(&app, "/api/v2/server", Some(&token)).await;
    assert_eq!(
        body["data"]["server"]["default_stream_id"],
        streams[0]["id"]
    );
}

#[tokio::test]
async fn stream_crud_and_archive() {
    let (app, token, _, _) = build_two_with_domains().await;

    // create
    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Broadcasts", "stream_type": "broadcast" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["stream"]["permalink"], "broadcasts");
    assert_eq!(body["data"]["stream"]["stream_type"], "broadcast");

    // duplicate permalink → 422
    let (status, _) = post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Broadcasts" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // invalid type → 422
    let (status, _) = post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "X", "stream_type": "nonsense" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // show
    let (status, body) = request(&app, "/api/v2/server/streams/broadcasts", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["stream"]["name"], "Broadcasts");

    // update (rename)
    let (status, body) = patch_json(
        &app,
        "/api/v2/server/streams/broadcasts",
        &token,
        json!({ "name": "Newsletters" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["stream"]["name"], "Newsletters");

    // archive
    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams/broadcasts/archive",
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["stream"]["archived"], true);
}

#[tokio::test]
async fn send_targets_a_stream_and_list_filters_by_it() {
    let (app, token, _, store) = build_two_with_domains().await;
    // a second stream to route to
    post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Broadcasts", "stream_type": "broadcast" }),
    )
    .await;

    // send explicitly to the broadcasts stream
    let (status, body) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@alpha.example",
            "to": ["a@dest.example"],
            "subject": "Promo",
            "text_body": "x",
            "stream": "broadcasts"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let broadcast_id = body["data"]["message_id"].as_i64().unwrap();

    // send to the default stream (no stream param)
    send_one(
        &app,
        &token,
        "news@alpha.example",
        "b@dest.example",
        "Receipt",
        "t",
    )
    .await;

    // the broadcast message carries the broadcasts stream id
    let streams_id = {
        let (_, body) = request(&app, "/api/v2/server/streams/broadcasts", Some(&token)).await;
        body["data"]["stream"]["id"].as_i64().unwrap()
    };
    let stored = store.message_for(
        store
            .server_for_api_token(&token)
            .await
            .unwrap()
            .unwrap()
            .id,
        broadcast_id,
    );
    assert_eq!(stored.unwrap().stream_id, Some(streams_id as u64));

    // ?stream= filters the message list
    let (_, body) = request(
        &app,
        "/api/v2/server/messages?stream=broadcasts",
        Some(&token),
    )
    .await;
    let messages = body["data"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["subject"], "Promo");

    // sending to an unknown stream is a 422
    let (status, _) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({ "from": "news@alpha.example", "to": ["c@dest.example"], "subject": "x", "text_body": "y", "stream": "ghost" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ------------------------------------------- P6: inbound management

/// Seed an incoming message directly (inbound normally arrives via SMTP).
fn seed_incoming(store: &MS, server_id: u64, rcpt_to: &str, subject: &str) -> i64 {
    let raw = format!("Subject: {subject}\r\n\r\nbody\r\n").into_bytes();
    store
        .insert_message_record(camelmailer_core::QueuedMessage {
            server_id,
            rcpt_to: rcpt_to.into(),
            mail_from: "sender@remote.example".into(),
            raw_message: raw,
            received_with_ssl: false,
            scope: camelmailer_core::MessageScope::Incoming,
            bounce: false,
            domain_id: None,
            credential_id: None,
            route_id: None,
            tag: None,
            metadata: None,
            stream_id: None,
        })
        .id
}

#[tokio::test]
async fn inbound_list_show_and_scope_exclusion() {
    let (app, token, _, store) = build_two_with_domains().await;
    let server_id = store
        .server_for_api_token(&token)
        .await
        .unwrap()
        .unwrap()
        .id;
    let inbound_id = seed_incoming(&store, server_id, "support@alpha.example", "Help");
    // an outbound message must NOT appear in the inbound list
    send_one(
        &app,
        &token,
        "news@alpha.example",
        "x@dest.example",
        "Out",
        "t",
    )
    .await;

    let (status, body) = request(&app, "/api/v2/server/inbound", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let inbound = body["data"]["inbound"].as_array().unwrap();
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0]["subject"], "Help");
    assert_eq!(inbound[0]["scope"], "incoming");

    let (status, body) = request(
        &app,
        &format!("/api/v2/server/inbound/{inbound_id}"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["message"]["subject"], "Help");
}

#[tokio::test]
async fn inbound_bypass_and_retry_requeue_the_message() {
    let (app, token, _, store) = build_two_with_domains().await;
    let server_id = store
        .server_for_api_token(&token)
        .await
        .unwrap()
        .unwrap()
        .id;
    let id = seed_incoming(&store, server_id, "support@alpha.example", "Ticket");
    store.set_message_status(id, "HardFail");

    // bypass resets status to Pending and flags the message
    let (status, body) = post_json(
        &app,
        &format!("/api/v2/server/inbound/{id}/bypass"),
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["requeued"], true);
    assert_eq!(body["data"]["message"]["status"], "Pending");
    assert_eq!(body["data"]["message"]["bypassed"], true);

    // retry resets status again (bypassed stays set)
    store.set_message_status(id, "HardFail");
    let (status, body) = post_json(
        &app,
        &format!("/api/v2/server/inbound/{id}/retry"),
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["message"]["status"], "Pending");

    // an outbound message can't be retried via the inbound endpoint
    let out = send_one(
        &app,
        &token,
        "news@alpha.example",
        "y@dest.example",
        "Out",
        "t",
    )
    .await;
    let (status, _) = post_json(
        &app,
        &format!("/api/v2/server/inbound/{out}/retry"),
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn inbound_is_tenant_scoped() {
    let (app, token_a, token_b, store) = build_two_with_domains().await;
    let server_a = store
        .server_for_api_token(&token_a)
        .await
        .unwrap()
        .unwrap()
        .id;
    let id = seed_incoming(&store, server_a, "support@alpha.example", "Private");

    // token B sees no inbound and cannot read/act on A's message
    let (_, body) = request(&app, "/api/v2/server/inbound", Some(&token_b)).await;
    assert_eq!(body["data"]["inbound"].as_array().unwrap().len(), 0);
    let (status, _) = request(
        &app,
        &format!("/api/v2/server/inbound/{id}"),
        Some(&token_b),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = post_json(
        &app,
        &format!("/api/v2/server/inbound/{id}/bypass"),
        &token_b,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ------------------------------------------- P7: templates + rendering

#[tokio::test]
async fn template_crud_render_and_scoping() {
    let (app, token_a, token_b, _) = build_two_with_domains().await;

    // create
    let (status, body) = post_json(
        &app,
        "/api/v2/server/templates",
        &token_a,
        json!({
            "name": "Welcome",
            "subject": "Hi {{ name }}",
            "html_body": "<p>Hi {{ name }}</p>",
            "text_body": "Hi {{ name }}"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["template"]["permalink"], "welcome");

    // list + show
    let (_, body) = request(&app, "/api/v2/server/templates", Some(&token_a)).await;
    assert_eq!(body["data"]["templates"].as_array().unwrap().len(), 1);
    let (status, _) = request(&app, "/api/v2/server/templates/welcome", Some(&token_a)).await;
    assert_eq!(status, StatusCode::OK);

    // dry-run render escapes by default
    let (status, body) = post_json(
        &app,
        "/api/v2/server/templates/welcome/render",
        &token_a,
        json!({ "template_model": { "name": "<b>Ada</b>" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["rendered"]["subject"],
        "Hi &lt;b&gt;Ada&lt;/b&gt;"
    );

    // update + archive
    let (status, body) = patch_json(
        &app,
        "/api/v2/server/templates/welcome",
        &token_a,
        json!({ "subject": "Hello {{ name }}" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["template"]["subject"], "Hello {{ name }}");
    let (status, body) = post_json(
        &app,
        "/api/v2/server/templates/welcome/archive",
        &token_a,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["template"]["archived"], true);

    // tenant B cannot see or read A's template
    let (_, body) = request(&app, "/api/v2/server/templates", Some(&token_b)).await;
    assert_eq!(body["data"]["templates"].as_array().unwrap().len(), 0);
    let (status, _) = request(&app, "/api/v2/server/templates/welcome", Some(&token_b)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn send_with_template_renders_and_enqueues() {
    let (app, token, _, store) = build_two_with_domains().await;
    post_json(
        &app,
        "/api/v2/server/templates",
        &token,
        json!({
            "name": "Receipt",
            "subject": "Order {{ order.id }}",
            "text_body": "Thanks {{ name }}"
        }),
    )
    .await;

    let (status, body) = post_json(
        &app,
        "/api/v2/server/messages/with_template",
        &token,
        json!({
            "from": "news@alpha.example",
            "to": ["buyer@dest.example"],
            "template": "receipt",
            "template_model": { "name": "Ada", "order": { "id": 42 } }
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["data"]["message_id"].is_number());

    // the enqueued MIME carries the rendered subject + body
    let server_id = store
        .server_for_api_token(&token)
        .await
        .unwrap()
        .unwrap()
        .id;
    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 1);
    let raw = String::from_utf8_lossy(&stored[0].raw_message);
    assert!(
        raw.contains("Subject: Order 42"),
        "subject not rendered: {raw}"
    );
    assert!(raw.contains("Thanks Ada"));

    // unknown template → 422
    let (status, _) = post_json(
        &app,
        "/api/v2/server/messages/with_template",
        &token,
        json!({ "from": "news@alpha.example", "to": ["x@dest.example"], "template": "ghost" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn streams_are_tenant_scoped() {
    let (app, token_a, token_b, _) = build_two_with_domains().await;
    post_json(
        &app,
        "/api/v2/server/streams",
        &token_a,
        json!({ "name": "Secret" }),
    )
    .await;
    // token B never sees server A's stream
    let (_, body) = request(&app, "/api/v2/server/streams", Some(&token_b)).await;
    let permalinks: Vec<&str> = body["data"]["streams"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["permalink"].as_str().unwrap())
        .collect();
    assert!(!permalinks.contains(&"secret"));
    let (status, _) = request(&app, "/api/v2/server/streams/secret", Some(&token_b)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
