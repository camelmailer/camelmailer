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

// ------------------------------------------------ P3: message read APIs

/// Two servers, each with a verified domain + API token, so cross-tenant
/// isolation can be exercised. Returns (router, token_a, token_b, store).
async fn build_two_with_domains() -> (Router, String, String, Arc<MS>) {
    let store = Arc::new(MS::new());
    let org = store
        .create_organization(NewOrganization { name: "Org".into(), permalink: "org".into() })
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
    (build_server_router(state), tokens[0].clone(), tokens[1].clone(), store)
}

/// Send one message and return the stored message id.
async fn send_one(app: &Router, token: &str, from: &str, to: &str, subject: &str, tag: &str) -> i64 {
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
    send_one(&app, &token, "news@alpha.example", "one@dest.example", "First", "welcome").await;
    send_one(&app, &token, "news@alpha.example", "two@dest.example", "Second", "promo").await;

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
    let (_, body) = request(&app, "/api/v2/server/messages?per_page=1&page=2", Some(&token)).await;
    assert_eq!(body["data"]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["pagination"]["total_pages"], 2);
}

#[tokio::test]
async fn message_show_includes_deliveries_opens_and_clicks() {
    let (app, token, _, store) = build_two_with_domains().await;
    let id = send_one(&app, &token, "news@alpha.example", "one@dest.example", "Hi", "t").await;

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
    let (status, body) = request(&app, &format!("/api/v2/server/messages/{id}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["message"]["subject"], "Hi");
    assert_eq!(body["data"]["message"]["status"], "Pending");
    assert_eq!(body["data"]["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["deliveries"][0]["status"], "Sent");

    // dedicated activity endpoints
    let (_, body) = request(&app, &format!("/api/v2/server/messages/{id}/deliveries"), Some(&token)).await;
    assert_eq!(body["data"]["deliveries"].as_array().unwrap().len(), 1);
    let (_, body) = request(&app, &format!("/api/v2/server/messages/{id}/opens"), Some(&token)).await;
    assert_eq!(body["data"]["opens"].as_array().unwrap().len(), 1);
    let (_, body) = request(&app, &format!("/api/v2/server/messages/{id}/clicks"), Some(&token)).await;
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
    let id = send_one(&app, &token, "news@alpha.example", "one@dest.example", "Hi", "t").await;

    let (status, body) = request(&app, &format!("/api/v2/server/messages/{id}/raw"), Some(&token)).await;
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
    let (status, body) = request(&app, &format!("/api/v2/server/messages/{id}/raw"), Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotAvailable");
}

#[tokio::test]
async fn a_server_token_cannot_read_another_servers_message() {
    let (app, token_a, token_b, _) = build_two_with_domains().await;
    let id = send_one(&app, &token_a, "news@alpha.example", "one@dest.example", "Secret", "t").await;

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
    let a = send_one(&app, &token, "news@alpha.example", "one@dest.example", "A", "t").await;
    let b = send_one(&app, &token, "news@alpha.example", "two@dest.example", "B", "t").await;

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
    let normal = send_one(&app, &token, "news@alpha.example", "one@dest.example", "OK", "t").await;
    let bounced = send_one(&app, &token, "news@alpha.example", "two@dest.example", "Bad", "t").await;
    store.set_message_status(bounced, "Bounced");

    // list contains only the bounced message
    let (status, body) = request(&app, "/api/v2/server/bounces", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let bounces = body["data"]["bounces"].as_array().unwrap();
    assert_eq!(bounces.len(), 1);
    assert_eq!(bounces[0]["subject"], "Bad");

    // show works for the bounce, 404 for a non-bounce
    let (status, _) = request(&app, &format!("/api/v2/server/bounces/{bounced}"), Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(&app, &format!("/api/v2/server/bounces/{normal}"), Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stats_and_bounces_are_tenant_scoped() {
    let (app, token_a, token_b, store) = build_two_with_domains().await;
    let id = send_one(&app, &token_a, "news@alpha.example", "one@dest.example", "A", "t").await;
    store.set_message_status(id, "Bounced");

    // token B sees an empty stat line and no bounces
    let (_, body) = request(&app, "/api/v2/server/stats", Some(&token_b)).await;
    assert_eq!(body["data"]["stats"]["total"], 0);
    let (_, body) = request(&app, "/api/v2/server/bounces", Some(&token_b)).await;
    assert_eq!(body["data"]["bounces"].as_array().unwrap().len(), 0);
    let (status, _) = request(&app, &format!("/api/v2/server/bounces/{id}"), Some(&token_b)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
