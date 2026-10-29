//! Captcha at self-registration, against a local stand-in for a Cap
//! instance's `siteverify` endpoint.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use camelmailer_api::{build_auth_router, ApiState};
use camelmailer_core::{AdminStore, MemoryStore};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

const GOOD_TOKEN: &str = "good-token";

/// A siteverify endpoint at `/{site_key}/siteverify` that accepts exactly
/// `GOOD_TOKEN` with secret `shh`, and records every body it saw.
async fn start_cap(broken: bool) -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let log = seen.clone();
    let app = Router::new().route(
        "/site-key-1/siteverify",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                log.lock().unwrap().push(body.clone());
                if broken {
                    return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({})));
                }
                let ok = body["secret"] == json!("shh") && body["response"] == json!(GOOD_TOKEN);
                (StatusCode::OK, Json(json!({ "success": ok })))
            }
        }),
    );
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("{base}/site-key-1/"), seen)
}

fn app(endpoint: Option<&str>) -> (Router, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::new());
    let mut config = camelmailer_config::Config::default();
    config.auth.allow_registration = true;
    if let Some(endpoint) = endpoint {
        config.captcha = serde_yaml::from_str(&format!(
            "provider: cap\nendpoint: {endpoint}\nsecret: shh\n"
        ))
        .unwrap();
    }
    config.validate().unwrap();
    let state = ApiState::full(
        store.clone(),
        Some(store.clone()),
        Some(store.clone()),
        None,
        config,
    );
    (build_auth_router(state), store)
}

async fn call(app: &Router, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    let body = match body {
        Some(body) => {
            builder = builder.header("content-type", "application/json");
            Body::from(body.to_string())
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

fn registration(token: Option<&str>) -> Value {
    let mut body = json!({
        "email_address": "ada@example.com",
        "first_name": "Ada",
        "last_name": "Lovelace",
        "password": "a-long-enough-password",
    });
    if let Some(token) = token {
        body["captcha_token"] = json!(token);
    }
    body
}

#[tokio::test]
async fn without_a_captcha_registration_needs_no_token() {
    let (app, _) = app(None);
    let (status, body) = call(
        &app,
        "POST",
        "/api/v2/auth/register",
        Some(registration(None)),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn cap_requires_a_token_and_verifies_it_server_side() {
    let (endpoint, seen) = start_cap(false).await;
    let (app, store) = app(Some(&endpoint));

    let (status, body) = call(
        &app,
        "POST",
        "/api/v2/auth/register",
        Some(registration(None)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("ParameterMissing"));

    let (status, body) = call(
        &app,
        "POST",
        "/api/v2/auth/register",
        Some(registration(Some("forged"))),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"]["code"], json!("CaptchaInvalid"));
    assert!(
        store.list_users().await.unwrap().is_empty(),
        "nothing created"
    );

    let (status, body) = call(
        &app,
        "POST",
        "/api/v2/auth/register",
        Some(registration(Some(GOOD_TOKEN))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // The verifier got the secret and the token, in Cap's field names.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1], json!({ "secret": "shh", "response": GOOD_TOKEN }));
}

#[tokio::test]
async fn the_widget_field_name_is_accepted_too() {
    let (endpoint, _) = start_cap(false).await;
    let (app, _) = app(Some(&endpoint));
    let mut body = registration(None);
    body["cap-token"] = json!(GOOD_TOKEN);
    let (status, body) = call(&app, "POST", "/api/v2/auth/register", Some(body)).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
}

#[tokio::test]
async fn an_unreachable_verifier_is_reported_as_unavailable() {
    let (endpoint, _) = start_cap(true).await;
    let (app, _) = app(Some(&endpoint));
    let (status, body) = call(
        &app,
        "POST",
        "/api/v2/auth/register",
        Some(registration(Some(GOOD_TOKEN))),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], json!("CaptchaUnavailable"));
}

#[tokio::test]
async fn features_expose_the_endpoint_but_never_the_secret() {
    let (endpoint, _) = start_cap(false).await;
    let (app, _) = app(Some(endpoint.trim_end_matches('/')));
    let (status, body) = call(&app, "GET", "/api/v2/auth/features", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["captcha"]["provider"], json!("cap"));
    // normalized to one trailing slash
    assert_eq!(body["data"]["captcha"]["endpoint"], json!(endpoint));
    assert!(!body.to_string().contains("shh"));
}
