//! Webhook test sends (`POST …/webhooks/{id}/test`), exercised against a
//! local axum mock endpoint: success, HTTP failure, transport timeout,
//! and the validation/404 paths.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use axum::Router;
use camelmailer_api::{build_router, ApiState, ReqwestWebhookSender};
use camelmailer_core::{
    AdminStore, MemoryStore, NewOrganization, NewServer, NewWebhook, ServerMode, StaticDnsResolver,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const ADMIN_KEY: &str = "test-admin-key";

#[derive(Clone, Default)]
struct Recorded {
    requests: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

/// Start the local mock webhook endpoint: `/ok` records and answers 200,
/// `/fail` answers 500, `/hang` sleeps past any test timeout.
async fn start_mock_endpoint() -> (String, Recorded) {
    let recorded = Recorded::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());

    let app = Router::new()
        .route(
            "/ok",
            post(
                |State(recorded): State<Recorded>, headers: HeaderMap, body: String| async move {
                    let json: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    recorded.requests.lock().unwrap().push((headers, json));
                    StatusCode::OK
                },
            ),
        )
        .route(
            "/fail",
            post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route(
            "/hang",
            post(|| async {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                StatusCode::OK
            }),
        )
        .with_state(recorded.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (base, recorded)
}

/// Admin router + one org/server/webhook pointing at `url`.
async fn build(url: &str, sender_timeout: Option<std::time::Duration>) -> (Router, u64) {
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
            name: "Alpha".into(),
            permalink: "alpha".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let webhook = store
        .create_webhook(NewWebhook {
            server_id: server.id,
            name: "hook".into(),
            url: url.into(),
            all_events: true,
            sign: false,
            events: vec![],
            headers: std::collections::BTreeMap::from([(
                "Authorization".to_string(),
                "Bearer hook-secret".to_string(),
            )]),
        })
        .await
        .unwrap();

    let sender = match sender_timeout {
        Some(timeout) => ReqwestWebhookSender::with_timeout(timeout),
        None => ReqwestWebhookSender::new(),
    };
    let state = ApiState::full_with_webhook_sender(
        store.clone(),
        None,
        None,
        Some(ADMIN_KEY.into()),
        camelmailer_config::Config::default(),
        None,
        Arc::new(StaticDnsResolver::new()),
        Arc::new(camelmailer_api::HttpGithub::default()),
        Arc::new(sender),
    );
    (build_router(state), webhook.id)
}

async fn post_test(app: &Router, webhook_id: u64, body: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/v2/admin/organizations/org/servers/alpha/webhooks/{webhook_id}/test"
                ))
                .header("X-Admin-API-Key", ADMIN_KEY)
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
async fn a_successful_test_send_reports_delivery_and_hits_the_endpoint() {
    let (base, recorded) = start_mock_endpoint().await;
    let (app, webhook_id) = build(&format!("{base}/ok"), None).await;

    let (status, body) = post_test(&app, webhook_id, json!({ "event": "MessageSent" })).await;
    assert_eq!(status, StatusCode::OK);
    let result = &body["data"]["result"];
    assert_eq!(result["delivered"], true);
    assert_eq!(result["status_code"], 200);
    assert!(result["duration_ms"].is_number());
    assert!(result["error"].is_null());

    // the endpoint saw exactly one request, with the platform headers,
    // the custom header, and a payload marked as a test
    let requests = recorded.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let (headers, payload) = &requests[0];
    assert_eq!(headers["x-camelmailer-event"], "MessageSent");
    assert!(headers.contains_key("x-camelmailer-uuid"));
    assert_eq!(headers["authorization"], "Bearer hook-secret");
    assert_eq!(headers["content-type"], "application/json");
    // sign=false and no installation key → no signature header
    assert!(!headers.contains_key("x-camelmailer-signature"));
    assert_eq!(payload["test"], true);
    assert_eq!(payload["event"], "MessageSent");
    assert_eq!(
        payload["payload"]["message"]["rcpt_to"],
        "recipient@example.com"
    );
    assert!(payload["payload"]["details"].is_string());
}

#[tokio::test]
async fn an_http_error_reports_the_status_without_delivery() {
    let (base, _) = start_mock_endpoint().await;
    let (app, webhook_id) = build(&format!("{base}/fail"), None).await;
    let (status, body) = post_test(&app, webhook_id, json!({ "event": "MessageHeld" })).await;
    assert_eq!(status, StatusCode::OK);
    let result = &body["data"]["result"];
    assert_eq!(result["delivered"], false);
    assert_eq!(result["status_code"], 500);
    assert!(result["error"].is_null());
}

#[tokio::test]
async fn a_timeout_reports_a_transport_error() {
    let (base, _) = start_mock_endpoint().await;
    let (app, webhook_id) = build(
        &format!("{base}/hang"),
        Some(std::time::Duration::from_millis(300)),
    )
    .await;
    let (status, body) = post_test(&app, webhook_id, json!({ "event": "MessageDelayed" })).await;
    assert_eq!(status, StatusCode::OK);
    let result = &body["data"]["result"];
    assert_eq!(result["delivered"], false);
    assert!(result["status_code"].is_null());
    assert!(result["error"].is_string());
}

#[tokio::test]
async fn invalid_or_missing_events_are_rejected() {
    let (base, _) = start_mock_endpoint().await;
    let (app, webhook_id) = build(&format!("{base}/ok"), None).await;

    let (status, body) = post_test(&app, webhook_id, json!({ "event": "NotAnEvent" })).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("MessageSent"));

    let (status, body) = post_test(&app, webhook_id, json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "ParameterMissing");
}

#[tokio::test]
async fn unknown_webhooks_are_not_found() {
    let (base, _) = start_mock_endpoint().await;
    let (app, _) = build(&format!("{base}/ok"), None).await;
    let (status, body) = post_test(&app, 424242, json!({ "event": "MessageSent" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
}
