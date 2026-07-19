//! Non-sending historical message import:
//! `POST /api/v2/admin/organizations/{org}/servers/{server}/messages/import`.
//! Imports past messages (from a Postal migration) as completed records that
//! are NEVER queued or delivered. Covers the happy path (read back via the
//! messaging read API), per-item failures, and the batch-size guard.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::Engine;
use camelmailer_api::{build_router, build_server_router, ApiState};
use camelmailer_core::{
    AdminStore, CredentialType, MemoryStore, NewCredential, NewOrganization, NewServer, ServerMode,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const ADMIN_KEY: &str = "admin-key-000000000000";
const SERVER_TOKEN: &str = "srv-tok-0000000000000";

struct Harness {
    app: Router,
}

async fn harness() -> Harness {
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
            name: "Alpha".into(),
            permalink: "alpha".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    // A server API token so the imported message can be read back over the
    // messaging surface (`/api/v2/server`).
    store
        .create_credential_record(NewCredential {
            server_id: server.id,
            credential_type: CredentialType::Api,
            name: "api".into(),
            key: Some(SERVER_TOKEN.into()),
        })
        .await
        .unwrap();

    let state = ApiState::full(
        store.clone(),
        Some(store.clone()),
        Some(store.clone()),
        Some(ADMIN_KEY.into()),
        camelmailer_config::Config::default(),
    );
    let app = build_router(state.clone()).merge(build_server_router(state));
    Harness { app }
}

impl Harness {
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, value);
        }
        let body = match body {
            Some(value) => {
                builder = builder.header("content-type", "application/json");
                Body::from(value.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .app
            .clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json)
    }

    async fn import(&self, body: Value) -> (StatusCode, Value) {
        self.request(
            "POST",
            "/api/v2/admin/organizations/acme/servers/alpha/messages/import",
            &[("X-Admin-API-Key", ADMIN_KEY.to_string())],
            Some(body),
        )
        .await
    }
}

fn b64(raw: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(raw.as_bytes())
}

#[tokio::test]
async fn imports_a_historical_message_and_reads_it_back_without_sending() {
    let h = harness().await;
    let raw = "Subject: Historical\r\nMessage-ID: <h1@org.example>\r\n\r\nBody\r\n";
    let (status, body) = h
        .import(json!({
            "messages": [{
                "scope": "outgoing",
                "mail_from": "from@example.com",
                "rcpt_to": "user@example.net",
                "raw_message_base64": b64(raw),
                "received_with_ssl": true,
                "bounce": false,
                "tag": "migrated",
                "timestamp": "2024-01-02T03:04:05Z",
                "deliveries": [
                    {"status": "Sent", "details": null, "output": "250 OK",
                     "sent_with_ssl": true, "timestamp": 1704164645}
                ],
                "opens": [{"timestamp": 1704164700, "ip": "1.2.3.4", "user_agent": "UA/1.0"}],
                "clicks": [{"url": "https://example.com/offer", "timestamp": 1704164800}]
            }]
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["imported"], 1);
    assert_eq!(body["data"]["failed"].as_array().unwrap().len(), 0);

    // Read the message back over the messaging API: it appears with the
    // delivery-derived status "Sent".
    let (status, body) = h
        .request(
            "GET",
            "/api/v2/server/messages",
            &[("X-Server-API-Key", SERVER_TOKEN.to_string())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let messages = body["data"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["status"], "Sent");
    assert_eq!(messages[0]["rcpt_to"], "user@example.net");
    let id = messages[0]["id"].as_i64().unwrap();

    // Detail endpoint shows the imported delivery and open.
    let (status, body) = h
        .request(
            "GET",
            &format!("/api/v2/server/messages/{id}"),
            &[("X-Server-API-Key", SERVER_TOKEN.to_string())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["message"]["status"], "Sent");
    assert_eq!(body["data"]["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["deliveries"][0]["status"], "Sent");

    let (status, body) = h
        .request(
            "GET",
            &format!("/api/v2/server/messages/{id}/opens"),
            &[("X-Server-API-Key", SERVER_TOKEN.to_string())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["opens"].as_array().unwrap().len(), 1);

    // NOTHING was queued/sent: the delivery-queue view stays empty.
    let (status, body) = h
        .request(
            "GET",
            "/api/v2/server/stats/deliveries",
            &[("X-Server-API-Key", SERVER_TOKEN.to_string())],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["queued"], 0);
}

#[tokio::test]
async fn per_item_failures_are_reported_and_do_not_abort_the_batch() {
    let h = harness().await;
    let raw = "Subject: Ok\r\n\r\nBody\r\n";
    let (status, body) = h
        .import(json!({
            "messages": [
                // valid
                {"scope": "outgoing", "mail_from": "a@x.com", "rcpt_to": "b@y.net",
                 "raw_message_base64": b64(raw), "timestamp": 1704164645, "deliveries": []},
                // invalid delivery status
                {"scope": "outgoing", "mail_from": "a@x.com", "rcpt_to": "c@y.net",
                 "raw_message_base64": b64(raw), "timestamp": 1704164645,
                 "deliveries": [{"status": "Delivered", "timestamp": 1704164645}]},
                // bad base64
                {"scope": "outgoing", "mail_from": "a@x.com", "rcpt_to": "d@y.net",
                 "raw_message_base64": "!!!not-base64!!!", "timestamp": 1704164645},
                // bad scope
                {"scope": "sideways", "mail_from": "a@x.com", "rcpt_to": "e@y.net",
                 "raw_message_base64": b64(raw), "timestamp": 1704164645}
            ]
        }))
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["imported"], 1);
    let failed = body["data"]["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 3);
    let indices: Vec<i64> = failed
        .iter()
        .map(|f| f["index"].as_i64().unwrap())
        .collect();
    assert_eq!(indices, vec![1, 2, 3]);
}

#[tokio::test]
async fn over_the_batch_cap_is_a_validation_error() {
    let h = harness().await;
    let raw = b64("Subject: X\r\n\r\nB\r\n");
    let messages: Vec<Value> = (0..501)
        .map(|i| {
            json!({
                "scope": "outgoing",
                "mail_from": "a@x.com",
                "rcpt_to": format!("u{i}@y.net"),
                "raw_message_base64": raw,
                "timestamp": 1704164645
            })
        })
        .collect();
    let (status, body) = h.import(json!({ "messages": messages })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], "ValidationError");
}
