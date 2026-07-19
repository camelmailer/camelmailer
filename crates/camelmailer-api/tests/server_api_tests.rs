//! Per-server API (`/api/v2/server`) tests: server-token auth + scoping.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
use camelmailer_api::{build_server_router, run_scheduler_tick, ApiState};
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

    // opt the recipient in to the broadcast stream (Phase 4 gate)
    post_json(
        &app,
        "/api/v2/server/streams/broadcasts/subscribers",
        &token,
        json!({ "address": "a@dest.example" }),
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

// -------------------------- observability: tags, bounce categories, logs

#[tokio::test]
async fn tags_lists_recent_tags_with_counts_and_is_tenant_scoped() {
    let (app, token_a, token_b, store) = build_two_with_domains().await;
    for (subject, tag) in [("A1", "welcome"), ("A2", "welcome"), ("A3", "promo")] {
        send_one(
            &app,
            &token_a,
            "news@alpha.example",
            "one@dest.example",
            subject,
            tag,
        )
        .await;
    }
    // a tag last used outside the 30-day window is not listed
    let stale = send_one(
        &app,
        &token_a,
        "news@alpha.example",
        "one@dest.example",
        "Old",
        "stale",
    )
    .await;
    store.set_message_created_at(stale, chrono::Utc::now() - chrono::Duration::days(40));
    send_one(
        &app,
        &token_b,
        "news@beta.example",
        "two@dest.example",
        "B1",
        "beta-only",
    )
    .await;

    let (status, body) = request(&app, "/api/v2/server/tags", Some(&token_a)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["tags"],
        json!([
            { "tag": "welcome", "count": 2 },
            { "tag": "promo", "count": 1 },
        ])
    );

    // the other tenant sees only its own tags
    let (_, body) = request(&app, "/api/v2/server/tags", Some(&token_b)).await;
    assert_eq!(
        body["data"]["tags"],
        json!([{ "tag": "beta-only", "count": 1 }])
    );
}

#[tokio::test]
async fn stats_scope_to_a_tag() {
    let (app, token, _, store) = build_two_with_domains().await;
    let tagged = send_one(
        &app,
        &token,
        "news@alpha.example",
        "one@dest.example",
        "T",
        "receipt",
    )
    .await;
    store.set_message_status(tagged, "Sent");
    send_one(
        &app,
        &token,
        "news@alpha.example",
        "two@dest.example",
        "U",
        "other",
    )
    .await;

    let (status, body) = request(&app, "/api/v2/server/stats?tag=receipt", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["stats"]["total"], 1);
    assert_eq!(body["data"]["stats"]["sent"], 1);

    let (_, body) = request(&app, "/api/v2/server/stats?tag=missing", Some(&token)).await;
    assert_eq!(body["data"]["stats"]["total"], 0);

    // no tag = everything
    let (_, body) = request(&app, "/api/v2/server/stats", Some(&token)).await;
    assert_eq!(body["data"]["stats"]["total"], 2);
}

#[tokio::test]
async fn bounce_categories_are_exposed_and_broken_down_in_stats() {
    use camelmailer_core::bounce::BounceCategory;
    let (app, token, _, store) = build_two_with_domains().await;
    let send = |subject: &'static str| {
        send_one(
            &app,
            &token,
            "news@alpha.example",
            "one@dest.example",
            subject,
            "t",
        )
    };
    // a classified hard bounce (5xx reject)
    let hard = send("Hard").await;
    store.set_message_status(hard, "Bounced");
    store.set_bounce_category(hard, BounceCategory::Hard);
    // a terminal delivery failure classified soft (exhausted 4xx retries)
    let soft = send("Soft").await;
    store.set_message_status(soft, "HardFail");
    store.set_bounce_category(soft, BounceCategory::Soft);
    // an unclassified bounce counts as undetermined
    let unknown = send("Unknown").await;
    store.set_message_status(unknown, "Bounced");
    // a delivered message contributes to no bucket
    let ok = send("OK").await;
    store.set_message_status(ok, "Sent");

    // GET /bounces carries the category (null until classified)
    let (status, body) = request(&app, "/api/v2/server/bounces", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let bounces = body["data"]["bounces"].as_array().unwrap();
    let category_of = |subject: &str| {
        bounces
            .iter()
            .find(|b| b["subject"] == subject)
            .map(|b| b["bounce_category"].clone())
            .unwrap()
    };
    assert_eq!(category_of("Hard"), json!("hard"));
    assert_eq!(category_of("Unknown"), Value::Null);

    let (status, body) = request(
        &app,
        &format!("/api/v2/server/bounces/{hard}"),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["bounce"]["bounce_category"], "hard");

    // GET /stats breaks bounces down by category
    let (_, body) = request(&app, "/api/v2/server/stats", Some(&token)).await;
    assert_eq!(
        body["data"]["stats"]["bounces"],
        json!({ "hard": 1, "soft": 1, "undetermined": 1 })
    );
}

/// Poll `GET /logs` until at least `min` entries whose path contains
/// `needle` show up (the log write is fire-and-forget on a background
/// task). Panics after ~2s.
async fn wait_for_logged(app: &Router, token: &str, needle: &str, min: usize) -> Vec<Value> {
    for _ in 0..200 {
        let (_, body) = request(app, "/api/v2/server/logs?per_page=100", Some(token)).await;
        let matching: Vec<Value> = body["data"]["requests"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r["path"].as_str().unwrap_or("").contains(needle))
            .collect();
        if matching.len() >= min {
            return matching;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("request log never showed {min} entries matching {needle:?}");
}

#[tokio::test]
async fn request_log_records_authenticated_requests_asynchronously() {
    let (app, token, _, _) = build_two_with_domains().await;

    // a 2xx request, with an oversized user agent and a query string
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v2/server/messages?tag=secret-query")
                .header("X-Server-API-Key", &token)
                .header("User-Agent", "a".repeat(300))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // a 4xx request is logged just like a 2xx one
    let (status, _) = request(&app, "/api/v2/server/messages/999999", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let logged = wait_for_logged(&app, &token, "/messages", 2).await;
    // newest first
    assert_eq!(logged[0]["path"], "/api/v2/server/messages/999999");
    assert_eq!(logged[0]["status_code"], 404);
    assert_eq!(logged[0]["method"], "GET");
    assert_eq!(logged[1]["path"], "/api/v2/server/messages");
    assert_eq!(logged[1]["status_code"], 200);
    // no query strings, truncated user agent, a duration
    assert_eq!(logged[1]["user_agent"].as_str().unwrap().len(), 255);
    assert!(logged[1]["duration_ms"].as_i64().unwrap() >= 0);
    assert!(logged[1]["created_at"].is_string());
}

#[tokio::test]
async fn request_log_filters_by_status_class_and_method() {
    let (app, token, _, _) = build_two_with_domains().await;
    let (_, _) = request(&app, "/api/v2/server/messages", Some(&token)).await;
    let (_, _) = request(&app, "/api/v2/server/messages/999999", Some(&token)).await;
    wait_for_logged(&app, &token, "/messages", 2).await;

    let (status, body) = request(&app, "/api/v2/server/logs?status=4xx", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let requests = body["data"]["requests"].as_array().unwrap();
    assert!(!requests.is_empty());
    assert!(requests
        .iter()
        .all(|r| (400..500).contains(&r["status_code"].as_i64().unwrap())));

    let (_, body) = request(&app, "/api/v2/server/logs?method=post", Some(&token)).await;
    assert_eq!(body["data"]["requests"].as_array().unwrap().len(), 0);

    let (status, body) = request(&app, "/api/v2/server/logs?status=bogus", Some(&token)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
}

#[tokio::test]
async fn request_log_is_tenant_scoped_and_skips_unauthenticated_requests() {
    let (app, token_a, token_b, store) = build_two_with_domains().await;

    // server A traffic + an unauthenticated 401 (which must not be logged)
    let (_, _) = request(&app, "/api/v2/server/messages", Some(&token_a)).await;
    let (status, _) = request(&app, "/api/v2/server/messages", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    wait_for_logged(&app, &token_a, "/messages", 1).await;

    // the foreign server sees none of A's entries
    let (_, body) = request(&app, "/api/v2/server/logs?per_page=100", Some(&token_b)).await;
    let foreign = body["data"]["requests"].as_array().unwrap();
    assert!(foreign
        .iter()
        .all(|r| !r["path"].as_str().unwrap().contains("/messages")));

    // the 401 was never logged anywhere: no entry without a server, and
    // A's log holds exactly one /messages entry
    let logged = wait_for_logged(&app, &token_a, "/messages", 1).await;
    assert_eq!(logged.len(), 1);
    let _ = store;
}

// --------------------------------------------------- P7: layouts

#[tokio::test]
async fn layouts_wrap_rendered_templates_and_unhook_on_delete() {
    let (app, token_a, token_b, _) = build_two_with_domains().await;

    // an HTML wrapper without raw {{{ content }}} is rejected
    let (status, _) = post_json(
        &app,
        "/api/v2/server/layouts",
        &token_a,
        json!({ "name": "Broken", "html_wrapper": "<div>{{ content }}</div>" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // create a proper layout with brand chrome
    let (status, body) = post_json(
        &app,
        "/api/v2/server/layouts",
        &token_a,
        json!({
            "name": "Brand",
            "html_wrapper": "<header>{{ product }}</header>{{{ content }}}<footer>Acme GmbH · Camelweg 1</footer>",
            "text_wrapper": "{{& content }}\n--\nAcme GmbH"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["layout"]["permalink"], "brand");

    // a template referencing the layout by permalink
    let (status, body) = post_json(
        &app,
        "/api/v2/server/templates",
        &token_a,
        json!({
            "name": "Welcome",
            "subject": "Hi {{ name }}",
            "html_body": "<p>Hi {{ name }}</p>",
            "text_body": "Hi {{ name }}",
            "layout": "brand"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert!(body["data"]["template"]["layout_id"].as_u64().is_some());

    // a bad layout reference is a validation error
    let (status, _) = post_json(
        &app,
        "/api/v2/server/templates",
        &token_a,
        json!({ "name": "Bad", "layout": "missing" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // the dry-run render wraps html and text, subject stays bare
    let (status, body) = post_json(
        &app,
        "/api/v2/server/templates/welcome/render",
        &token_a,
        json!({ "template_model": { "name": "Ada", "product": "Acme" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["rendered"]["html_body"],
        "<header>Acme</header><p>Hi Ada</p><footer>Acme GmbH · Camelweg 1</footer>"
    );
    assert_eq!(
        body["data"]["rendered"]["text_body"],
        "Hi Ada\n--\nAcme GmbH"
    );
    assert_eq!(body["data"]["rendered"]["subject"], "Hi Ada");

    // tenant B sees no layouts
    let (_, body) = request(&app, "/api/v2/server/layouts", Some(&token_b)).await;
    assert_eq!(body["data"]["layouts"].as_array().unwrap().len(), 0);

    // detaching via PATCH layout: "" renders bare again
    let (status, body) = patch_json(
        &app,
        "/api/v2/server/templates/welcome",
        &token_a,
        json!({ "layout": "" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["data"]["template"]["layout_id"].is_null());

    // re-attach, then deleting the layout unhooks the template
    patch_json(
        &app,
        "/api/v2/server/templates/welcome",
        &token_a,
        json!({ "layout": "brand" }),
    )
    .await;
    let (status, _) = json_request(
        &app,
        "DELETE",
        "/api/v2/server/layouts/brand",
        &token_a,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = request(&app, "/api/v2/server/templates/welcome", Some(&token_a)).await;
    assert!(body["data"]["template"]["layout_id"].is_null());
    let (status, body) = post_json(
        &app,
        "/api/v2/server/templates/welcome/render",
        &token_a,
        json!({ "template_model": { "name": "Ada" } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["rendered"]["html_body"], "<p>Hi Ada</p>");
}

// ------------------------------------------ broadcast: List-Unsubscribe

#[tokio::test]
async fn broadcast_send_carries_list_unsubscribe_and_post_unsubscribes() {
    use camelmailer_core::{message::header_value, NewStream, ServerStore};

    let (server_app, token, store, server_id) = build_with_verified_domain().await;
    // The unsubscribe endpoint resolves through the same MemoryStore.
    let state = ApiState::with_server_store(store.clone(), store.clone(), None);
    let unsub_app = camelmailer_api::unsubscribe_router(state);

    // A broadcast stream to send on.
    ServerStore::create_stream(
        store.as_ref(),
        NewStream {
            server_id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();

    // opt the recipient in (Phase 4 broadcast gate)
    post_json(
        &server_app,
        "/api/v2/server/streams/broadcast/subscribers",
        &token,
        json!({ "address": "reader@dest.example" }),
    )
    .await;

    let (status, _) = post_json(
        &server_app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@org.example",
            "to": ["reader@dest.example"],
            "subject": "Weekly digest",
            "html_body": "<p>Hi</p>",
            "stream": "broadcast"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // The stored raw carries a one-click List-Unsubscribe header.
    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 1);
    let raw = &stored[0].raw_message;
    let lu = header_value(raw, "List-Unsubscribe").expect("List-Unsubscribe header present");
    assert!(lu.contains("/track/u/"), "unexpected header: {lu}");
    assert_eq!(
        header_value(raw, "List-Unsubscribe-Post").as_deref(),
        Some("List-Unsubscribe=One-Click")
    );

    // Extract the token from `<...://.../track/u/TOKEN>, <mailto:...>`.
    let token_value = lu
        .split("/track/u/")
        .nth(1)
        .and_then(|rest| rest.split('>').next())
        .expect("token in header")
        .to_string();

    // Not yet suppressed on the broadcast stream.
    let stream = ServerStore::stream_by_permalink(store.as_ref(), server_id, "broadcast")
        .await
        .unwrap()
        .unwrap();
    assert!(!ServerStore::address_suppressed(
        store.as_ref(),
        server_id,
        "reader@dest.example",
        Some(stream.id)
    )
    .await
    .unwrap());

    // One-click POST unsubscribes.
    let response = unsub_app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/track/u/{token_value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Now suppressed on the broadcast stream, but NOT server-wide / other streams.
    assert!(ServerStore::address_suppressed(
        store.as_ref(),
        server_id,
        "reader@dest.example",
        Some(stream.id)
    )
    .await
    .unwrap());
    assert!(!ServerStore::address_suppressed(
        store.as_ref(),
        server_id,
        "reader@dest.example",
        None
    )
    .await
    .unwrap());

    // An unknown token still returns a neutral 200 (no validity leak).
    let response = unsub_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/track/u/nope")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn broadcast_send_appends_compliance_footer() {
    use camelmailer_core::{mime::extract_bodies, AdminStore, NewStream, ServerStore};

    let (server_app, token, store, server_id) = build_with_verified_domain().await;

    // Configure the physical postal address for the CAN-SPAM footer.
    let mut server = AdminStore::server_for_api_token(store.as_ref(), &token)
        .await
        .unwrap()
        .unwrap();
    server.broadcast_physical_address = Some("Acme Inc, 1 Main St, Springfield".into());
    AdminStore::update_server(store.as_ref(), server)
        .await
        .unwrap();

    ServerStore::create_stream(
        store.as_ref(),
        NewStream {
            server_id,
            name: "Broadcast".into(),
            permalink: "broadcast".into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();

    // opt the recipient in (Phase 4 broadcast gate)
    post_json(
        &server_app,
        "/api/v2/server/streams/broadcast/subscribers",
        &token,
        json!({ "address": "reader@dest.example" }),
    )
    .await;

    let (status, _) = post_json(
        &server_app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@org.example",
            "to": ["reader@dest.example"],
            "subject": "Weekly digest",
            "html_body": "<p>Hi</p>",
            "text_body": "Hi",
            "stream": "broadcast"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 1);
    let bodies = extract_bodies(&stored[0].raw_message);

    let html = bodies.html.expect("html part present");
    assert!(
        html.contains("<p>Hi</p>"),
        "original html preserved: {html}"
    );
    assert!(
        html.contains("/track/u/"),
        "unsubscribe link in html: {html}"
    );
    assert!(
        html.contains("Unsubscribe"),
        "unsubscribe text in html: {html}"
    );
    assert!(
        html.contains("Acme Inc, 1 Main St, Springfield"),
        "physical address in html: {html}"
    );

    let text = bodies.text.expect("text part present");
    assert!(
        text.contains("Unsubscribe: "),
        "unsubscribe line in text: {text}"
    );
    assert!(
        text.contains("/track/u/"),
        "unsubscribe url in text: {text}"
    );
    assert!(
        text.contains("Acme Inc, 1 Main St, Springfield"),
        "physical address in text: {text}"
    );
}

#[tokio::test]
async fn transactional_send_has_no_list_unsubscribe() {
    use camelmailer_core::message::header_value;

    let (app, token, store, server_id) = build_with_verified_domain().await;
    let (status, _) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@org.example",
            "to": ["reader@dest.example"],
            "subject": "Receipt",
            "text_body": "thanks"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 1);
    assert!(header_value(&stored[0].raw_message, "List-Unsubscribe").is_none());
}

// -------------------------------------------- broadcast: opt-in / consent

#[tokio::test]
async fn broadcast_opt_in_gate_rejects_then_allows_then_rejects_after_unsubscribe() {
    use camelmailer_core::message::header_value;

    let (app, token, store, server_id) = build_with_verified_domain().await;
    // The unsubscribe endpoint resolves through the same MemoryStore.
    let state = ApiState::with_server_store(store.clone(), store.clone(), None);
    let unsub_app = camelmailer_api::unsubscribe_router(state);

    // a broadcast stream
    post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Broadcast", "stream_type": "broadcast" }),
    )
    .await;

    let send_body = json!({
        "from": "news@org.example",
        "to": ["reader@dest.example"],
        "subject": "Digest",
        "html_body": "<p>Hi</p>",
        "stream": "broadcast"
    });

    // 1) with no opt-in the whole send is rejected 422, naming the address
    //    and the stream, and nothing is stored.
    let (status, body) =
        post_json(&app, "/api/v2/server/messages", &token, send_body.clone()).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("reader@dest.example"),
        "message: {message}"
    );
    assert!(message.contains("broadcast"), "message: {message}");
    assert!(store.messages_for(server_id).is_empty());

    // 2) opting in through the subscribers API lets the send through.
    let (status, sub) = post_json(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        &token,
        json!({ "address": "reader@dest.example" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(sub["data"]["subscriber"]["status"], "subscribed");
    assert_eq!(sub["data"]["subscriber"]["address"], "reader@dest.example");

    let (status, _) = post_json(&app, "/api/v2/server/messages", &token, send_body.clone()).await;
    assert_eq!(status, StatusCode::CREATED);

    // 3) one-click unsubscribe flips the subscription; the next send is
    //    rejected again.
    let stored = store.messages_for(server_id);
    let raw = &stored.last().unwrap().raw_message;
    let lu = header_value(raw, "List-Unsubscribe").expect("List-Unsubscribe header");
    let token_value = lu
        .split("/track/u/")
        .nth(1)
        .and_then(|rest| rest.split('>').next())
        .expect("token in header")
        .to_string();
    let response = unsub_app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/track/u/{token_value}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // the subscription now reads `unsubscribed` in the list
    let (_, list) = request(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        Some(&token),
    )
    .await;
    let subscribers = list["data"]["subscribers"].as_array().unwrap();
    assert_eq!(subscribers.len(), 1);
    assert_eq!(subscribers[0]["status"], "unsubscribed");

    let (status, _) = post_json(&app, "/api/v2/server/messages", &token, send_body).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn subscriber_list_add_remove_round_trip() {
    let (app, token, _store, _server_id) = build_with_verified_domain().await;

    post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Broadcast", "stream_type": "broadcast" }),
    )
    .await;

    // empty to start
    let (status, list) = request(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["data"]["subscribers"].as_array().unwrap().len(), 0);

    // add two
    for addr in ["a@dest.example", "b@dest.example"] {
        let (status, _) = post_json(
            &app,
            "/api/v2/server/streams/broadcast/subscribers",
            &token,
            json!({ "address": addr }),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (_, list) = request(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        Some(&token),
    )
    .await;
    assert_eq!(list["data"]["subscribers"].as_array().unwrap().len(), 2);

    // upsert is idempotent (no duplicate row, status update in place)
    let (status, sub) = post_json(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        &token,
        json!({ "address": "a@dest.example", "status": "unsubscribed" }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(sub["data"]["subscriber"]["status"], "unsubscribed");
    let (_, list) = request(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        Some(&token),
    )
    .await;
    assert_eq!(list["data"]["subscribers"].as_array().unwrap().len(), 2);

    // remove one
    let (status, body) = json_request(
        &app,
        "DELETE",
        "/api/v2/server/streams/broadcast/subscribers/a@dest.example",
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["deleted"], true);
    let (_, list) = request(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        Some(&token),
    )
    .await;
    let subscribers = list["data"]["subscribers"].as_array().unwrap();
    assert_eq!(subscribers.len(), 1);
    assert_eq!(subscribers[0]["address"], "b@dest.example");

    // subscribers on an unknown stream -> 404
    let (status, _) = request(
        &app,
        "/api/v2/server/streams/nope/subscribers",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// -------------------------------------------- broadcast: campaign / import / complaint

/// Create a broadcast stream on the send-ready fixture server.
async fn create_broadcast_stream(store: &Arc<MS>, server_id: u64, permalink: &str) {
    use camelmailer_core::{NewStream, ServerStore};
    ServerStore::create_stream(
        store.as_ref(),
        NewStream {
            server_id,
            name: permalink.into(),
            permalink: permalink.into(),
            stream_type: "broadcast".into(),
            ip_pool_id: None,
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn campaign_send_reaches_every_subscriber_with_footer_and_unsubscribe() {
    use camelmailer_core::message::header_value;

    let (app, token, store, server_id) = build_with_verified_domain().await;
    create_broadcast_stream(&store, server_id, "broadcast").await;

    // Three opted-in subscribers.
    for address in ["a@dest.example", "b@dest.example", "c@dest.example"] {
        post_json(
            &app,
            "/api/v2/server/streams/broadcast/subscribers",
            &token,
            json!({ "address": address }),
        )
        .await;
    }

    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams/broadcast/send",
        &token,
        json!({
            "from": "news@org.example",
            "subject": "Weekly digest",
            "html_body": "<p>Hi</p>",
            "text_body": "Hi"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["queued"], 3);
    assert_eq!(body["data"]["skipped"], 0);

    // Every stored message carries a one-click List-Unsubscribe header and the
    // CAN-SPAM footer (baked into the raw by the shared broadcast path).
    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 3);
    for message in &stored {
        let raw = &message.raw_message;
        let lu = header_value(raw, "List-Unsubscribe").expect("List-Unsubscribe present");
        assert!(lu.contains("/track/u/"), "unexpected header: {lu}");
        let text = String::from_utf8_lossy(raw);
        assert!(text.contains("Unsubscribe"), "footer missing from {text}");
    }
}

#[tokio::test]
async fn campaign_send_on_a_non_broadcast_stream_is_rejected() {
    let (app, token, _store, _server_id) = build_with_verified_domain().await;
    // The server's default stream is transactional.
    post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Transact", "stream_type": "transactional" }),
    )
    .await;

    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams/transact/send",
        &token,
        json!({ "from": "news@org.example", "subject": "Hi", "html_body": "<p>x</p>" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"]["message"],
        "campaigns can only be sent on a broadcast stream"
    );
}

#[tokio::test]
async fn campaign_create_expands_asynchronously_and_reports_stats() {
    use camelmailer_core::message::header_value;

    let (app, token, store, server_id) = build_with_verified_domain().await;
    create_broadcast_stream(&store, server_id, "broadcast").await;

    // Four opted-in subscribers (plus one unsubscribed that must not count).
    for address in ["a@dest.example", "b@dest.example", "c@dest.example"] {
        post_json(
            &app,
            "/api/v2/server/streams/broadcast/subscribers",
            &token,
            json!({ "address": address }),
        )
        .await;
    }
    post_json(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        &token,
        json!({ "address": "gone@dest.example", "status": "unsubscribed" }),
    )
    .await;

    // Creating the campaign returns immediately (201) with the campaign; the
    // total is the current subscribed count (3), status starts `sending`.
    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams/broadcast/campaigns",
        &token,
        json!({
            "name": "Weekly digest",
            "from": "news@org.example",
            "subject": "Hello",
            "html_body": "<p>Hi</p>",
            "text_body": "Hi"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let campaign_id = body["data"]["campaign"]["id"].as_u64().unwrap();
    assert_eq!(body["data"]["campaign"]["total"], 3);
    assert!(matches!(
        body["data"]["campaign"]["status"].as_str(),
        Some("sending") | Some("sent")
    ));

    // The expansion runs on a spawned task; poll the campaign until it settles.
    let path = format!("/api/v2/server/streams/broadcast/campaigns/{campaign_id}");
    let mut settled = json!(null);
    for _ in 0..200 {
        tokio::task::yield_now().await;
        let (_, body) = request(&app, &path, Some(&token)).await;
        if body["data"]["campaign"]["status"] == "sent" {
            settled = body;
            break;
        }
    }
    assert_eq!(settled["data"]["campaign"]["status"], "sent");
    assert_eq!(settled["data"]["campaign"]["sent"], 3);
    assert_eq!(settled["data"]["stats"]["total"], 3);
    assert_eq!(settled["data"]["stats"]["sent"], 3);

    // Exactly three messages were produced, each carrying the broadcast
    // List-Unsubscribe header and the CAN-SPAM footer.
    let stored = store.messages_for(server_id);
    assert_eq!(stored.len(), 3);
    for message in &stored {
        let raw = &message.raw_message;
        assert!(header_value(raw, "List-Unsubscribe").is_some());
        assert!(String::from_utf8_lossy(raw).contains("Unsubscribe"));
    }

    // Each message is attributed to the campaign: marking them Sent makes the
    // per-campaign `delivered` counter reach 3 (it only counts attributed
    // messages).
    for message in &stored {
        store.set_message_status(message.id, "Sent");
    }
    let (_, body) = request(&app, &path, Some(&token)).await;
    assert_eq!(body["data"]["stats"]["delivered"], 3);
    assert_eq!(body["data"]["stats"]["failed"], 0);

    // The list endpoint surfaces the campaign.
    let (status, body) = request(
        &app,
        "/api/v2/server/streams/broadcast/campaigns",
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["campaigns"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn campaign_create_on_a_non_broadcast_stream_is_rejected() {
    let (app, token, _store, _server_id) = build_with_verified_domain().await;
    post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Transact", "stream_type": "transactional" }),
    )
    .await;

    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams/transact/campaigns",
        &token,
        json!({ "from": "news@org.example", "subject": "Hi", "html_body": "<p>x</p>" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"]["message"],
        "campaigns can only be created on a broadcast stream"
    );
}

// ------------------------------------ server-level campaigns (planning + scheduler)

/// Like [`build_with_verified_domain`] but also returns the `ApiState`, so a
/// test can drive the scheduler (`run_scheduler_tick`) directly.
async fn build_with_state() -> (Router, Arc<ApiState>, String, Arc<MS>, u64) {
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
    store.insert_domain(camelmailer_core::Domain {
        id: store.next_id(),
        uuid: "d".into(),
        owner: DomainOwner::Server(server.id),
        name: "org.example".into(),
        verified: true,
        verification_token: "vtoken".into(),
        dkim_private_key: None,
    });
    let token = "sched-token-000000000".to_string();
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
    let app = build_server_router(state.clone());
    (app, state, token, store, server.id)
}

/// Add `count` opted-in subscribers to a broadcast stream via the API.
async fn add_subscribers(app: &Router, token: &str, permalink: &str, addresses: &[&str]) {
    for address in addresses {
        post_json(
            app,
            &format!("/api/v2/server/streams/{permalink}/subscribers"),
            token,
            json!({ "address": address }),
        )
        .await;
    }
}

#[tokio::test]
async fn draft_campaign_is_not_expanded_then_send_expands() {
    let (app, token, store, server_id) = build_with_verified_domain().await;
    create_broadcast_stream(&store, server_id, "broadcast").await;
    add_subscribers(
        &app,
        &token,
        "broadcast",
        &["a@dest.example", "b@dest.example", "c@dest.example"],
    )
    .await;

    // A plain create (no send_now, no scheduled_at) is a draft; it must NOT
    // expand into any message.
    let (status, body) = post_json(
        &app,
        "/api/v2/server/campaigns",
        &token,
        json!({
            "stream": "broadcast",
            "name": "Draft digest",
            "from": "news@org.example",
            "subject": "Hello",
            "html_body": "<p>Hi</p>"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["campaign"]["status"], "draft");
    assert_eq!(body["data"]["campaign"]["stream"]["permalink"], "broadcast");
    let campaign_id = body["data"]["campaign"]["id"].as_u64().unwrap();
    // Give any (erroneously) spawned task a chance to run: still zero messages.
    tokio::task::yield_now().await;
    assert_eq!(store.messages_for(server_id).len(), 0);

    // Sending it now expands to all three subscribers.
    let path = format!("/api/v2/server/campaigns/{campaign_id}");
    let (status, body) = post_json(&app, &format!("{path}/send"), &token, json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(matches!(
        body["data"]["campaign"]["status"].as_str(),
        Some("sending") | Some("sent")
    ));

    let mut settled = json!(null);
    for _ in 0..200 {
        tokio::task::yield_now().await;
        let (_, body) = request(&app, &path, Some(&token)).await;
        if body["data"]["campaign"]["status"] == "sent" {
            settled = body;
            break;
        }
    }
    assert_eq!(settled["data"]["campaign"]["status"], "sent");
    assert_eq!(settled["data"]["campaign"]["sent"], 3);
    assert_eq!(store.messages_for(server_id).len(), 3);
}

#[tokio::test]
async fn scheduled_campaign_is_claimed_once_and_sent_by_the_scheduler() {
    use camelmailer_core::ServerStore;

    let (app, state, token, store, server_id) = build_with_state().await;
    create_broadcast_stream(&store, server_id, "broadcast").await;
    add_subscribers(
        &app,
        &token,
        "broadcast",
        &["a@dest.example", "b@dest.example"],
    )
    .await;

    // Schedule a campaign in the past — it is due immediately but must not have
    // expanded on create.
    let past = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    let (status, body) = post_json(
        &app,
        "/api/v2/server/campaigns",
        &token,
        json!({
            "stream": "broadcast",
            "from": "news@org.example",
            "subject": "Scheduled",
            "html_body": "<p>Hi</p>",
            "scheduled_at": past
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["campaign"]["status"], "scheduled");
    assert!(body["data"]["campaign"]["scheduled_at"].is_string());
    tokio::task::yield_now().await;
    assert_eq!(store.messages_for(server_id).len(), 0);

    // One scheduler pass claims and sends it to both subscribers.
    run_scheduler_tick(&state).await;
    assert_eq!(store.messages_for(server_id).len(), 2);
    let campaign_id = body["data"]["campaign"]["id"].as_u64().unwrap();
    let (_, shown) = request(
        &app,
        &format!("/api/v2/server/campaigns/{campaign_id}"),
        Some(&token),
    )
    .await;
    assert_eq!(shown["data"]["campaign"]["status"], "sent");
    assert_eq!(shown["data"]["campaign"]["sent"], 2);

    // A claim is exactly-once: a second pass finds nothing due and sends no
    // more messages.
    let none = ServerStore::claim_due_campaigns(store.as_ref(), server_id, chrono::Utc::now())
        .await
        .unwrap();
    assert!(none.is_empty());
    run_scheduler_tick(&state).await;
    assert_eq!(store.messages_for(server_id).len(), 2);
}

#[tokio::test]
async fn canceling_a_scheduled_campaign_stops_the_scheduler() {
    use camelmailer_core::ServerStore;

    let (app, state, token, store, server_id) = build_with_state().await;
    create_broadcast_stream(&store, server_id, "broadcast").await;
    add_subscribers(&app, &token, "broadcast", &["a@dest.example"]).await;

    let past = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    let (_, body) = post_json(
        &app,
        "/api/v2/server/campaigns",
        &token,
        json!({
            "stream": "broadcast",
            "from": "news@org.example",
            "subject": "Scheduled",
            "html_body": "<p>Hi</p>",
            "scheduled_at": past
        }),
    )
    .await;
    let campaign_id = body["data"]["campaign"]["id"].as_u64().unwrap();

    // Cancel it: status becomes canceled.
    let (status, body) = post_json(
        &app,
        &format!("/api/v2/server/campaigns/{campaign_id}/cancel"),
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["campaign"]["status"], "canceled");

    // The scheduler skips a canceled campaign — nothing is claimed or sent.
    let none = ServerStore::claim_due_campaigns(store.as_ref(), server_id, chrono::Utc::now())
        .await
        .unwrap();
    assert!(none.is_empty());
    run_scheduler_tick(&state).await;
    assert_eq!(store.messages_for(server_id).len(), 0);
}

#[tokio::test]
async fn editing_a_sent_campaign_is_rejected() {
    let (app, token, store, server_id) = build_with_verified_domain().await;
    create_broadcast_stream(&store, server_id, "broadcast").await;
    add_subscribers(&app, &token, "broadcast", &["a@dest.example"]).await;

    // Send now, then poll until it settles as sent.
    let (_, body) = post_json(
        &app,
        "/api/v2/server/campaigns",
        &token,
        json!({
            "stream": "broadcast",
            "from": "news@org.example",
            "subject": "Now",
            "html_body": "<p>Hi</p>",
            "send_now": true
        }),
    )
    .await;
    let campaign_id = body["data"]["campaign"]["id"].as_u64().unwrap();
    let path = format!("/api/v2/server/campaigns/{campaign_id}");
    for _ in 0..200 {
        tokio::task::yield_now().await;
        let (_, body) = request(&app, &path, Some(&token)).await;
        if body["data"]["campaign"]["status"] == "sent" {
            break;
        }
    }

    // A PATCH is rejected 422 once the campaign is no longer draft/scheduled.
    let (status, body) = patch_json(&app, &path, &token, json!({ "subject": "Edited" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"]["message"],
        "a sent campaign can no longer be edited"
    );
}

#[tokio::test]
async fn server_level_list_returns_campaigns_across_streams() {
    let (app, token, store, server_id) = build_with_verified_domain().await;
    create_broadcast_stream(&store, server_id, "one").await;
    create_broadcast_stream(&store, server_id, "two").await;

    for stream in ["one", "two"] {
        post_json(
            &app,
            "/api/v2/server/campaigns",
            &token,
            json!({
                "stream": stream,
                "from": "news@org.example",
                "subject": "Draft",
                "html_body": "<p>Hi</p>"
            }),
        )
        .await;
    }

    let (status, body) = request(&app, "/api/v2/server/campaigns", Some(&token)).await;
    assert_eq!(status, StatusCode::OK);
    let campaigns = body["data"]["campaigns"].as_array().unwrap();
    assert_eq!(campaigns.len(), 2);
    // Newest first, and each carries its audience stream's permalink.
    let permalinks: Vec<&str> = campaigns
        .iter()
        .map(|c| c["stream"]["permalink"].as_str().unwrap())
        .collect();
    assert!(permalinks.contains(&"one"));
    assert!(permalinks.contains(&"two"));
}

#[tokio::test]
async fn creating_a_campaign_on_a_non_broadcast_stream_is_rejected() {
    let (app, token, _store, _server_id) = build_with_verified_domain().await;
    post_json(
        &app,
        "/api/v2/server/streams",
        &token,
        json!({ "name": "Transact", "stream_type": "transactional" }),
    )
    .await;

    let (status, body) = post_json(
        &app,
        "/api/v2/server/campaigns",
        &token,
        json!({ "stream": "transact", "from": "news@org.example", "subject": "Hi" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        body["error"]["message"],
        "campaigns can only target a broadcast stream"
    );
}

#[tokio::test]
async fn subscriber_import_adds_and_dedupes() {
    let (app, token, _store, _server_id) = build_with_verified_domain().await;
    create_broadcast_stream(&_store, _server_id, "broadcast").await;

    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams/broadcast/subscribers/import",
        &token,
        json!({
            "addresses": [
                "a@dest.example",
                "b@dest.example",
                "a@dest.example",
                "  ",
                " c@dest.example "
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // a, b, c distinct non-blank -> 3 added; blanks and the dup skipped.
    assert_eq!(body["data"]["added"], 3);
    assert_eq!(body["data"]["total"], 3);

    // The list reflects them, trimmed.
    let (_, body) = request(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        Some(&token),
    )
    .await;
    let subs = body["data"]["subscribers"].as_array().unwrap();
    assert_eq!(subs.len(), 3);
    assert!(subs.iter().all(|s| s["status"] == "subscribed"));
    assert!(subs.iter().any(|s| s["address"] == "c@dest.example"));
}

#[tokio::test]
async fn complaint_suppresses_the_address_and_closes_the_opt_in_gate() {
    use camelmailer_core::ServerStore;

    let (app, token, store, server_id) = build_with_verified_domain().await;
    create_broadcast_stream(&store, server_id, "broadcast").await;

    post_json(
        &app,
        "/api/v2/server/streams/broadcast/subscribers",
        &token,
        json!({ "address": "reader@dest.example" }),
    )
    .await;

    let (status, body) = post_json(
        &app,
        "/api/v2/server/streams/broadcast/subscribers/reader@dest.example/complaint",
        &token,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["subscriber"]["status"], "unsubscribed");

    let stream = ServerStore::stream_by_permalink(store.as_ref(), server_id, "broadcast")
        .await
        .unwrap()
        .unwrap();
    // Stream-scoped complaint suppression exists; the opt-in gate is closed.
    assert!(!ServerStore::is_subscribed(
        store.as_ref(),
        server_id,
        stream.id,
        "reader@dest.example"
    )
    .await
    .unwrap());
    assert!(ServerStore::address_suppressed(
        store.as_ref(),
        server_id,
        "reader@dest.example",
        Some(stream.id)
    )
    .await
    .unwrap());
    let suppressions = store.list_suppressions(server_id).await.unwrap();
    assert_eq!(suppressions.len(), 1);
    assert_eq!(suppressions[0].suppression_type, "complaint");

    // A subsequent normal broadcast send to that address is rejected by the gate.
    let (status, _) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@org.example",
            "to": ["reader@dest.example"],
            "subject": "Hi",
            "html_body": "<p>x</p>",
            "stream": "broadcast"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

// ---------------------------------------------- P: send body-size limit

/// Build a server router whose send routes carry a body limit derived from a
/// deliberately small `smtp_server.max_message_size` (1 MB → a few-MB cap),
/// so the over-limit path is cheap to exercise. Returns (router, token).
async fn build_with_small_body_limit() -> (Router, String) {
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

    let mut config = camelmailer_config::Config::default();
    config.smtp_server.max_message_size = 1; // MB → limit ≈ 3 MiB
    let state = ApiState::full(store.clone(), Some(store.clone()), None, None, config);
    (build_server_router(state), token)
}

#[tokio::test]
async fn oversized_send_body_is_rejected() {
    let (app, token) = build_with_small_body_limit().await;
    // ~5 MiB body — over the ≈3 MiB cap for a 1 MB max_message_size.
    let huge = "x".repeat(5 * 1024 * 1024);
    let (status, _) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@org.example",
            "to": ["a@dest.example"],
            "subject": "Hi",
            "text_body": huge,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn in_limit_send_body_is_accepted() {
    let (app, token) = build_with_small_body_limit().await;
    let (status, body) = post_json(
        &app,
        "/api/v2/server/messages",
        &token,
        json!({
            "from": "news@org.example",
            "to": ["a@dest.example"],
            "subject": "Hi",
            "text_body": "a small body",
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(body["data"]["message_id"].is_number());
}
