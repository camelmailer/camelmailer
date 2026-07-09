//! Port of the Admin API v2 conventions covered by
//! `spec/apis/admin_api/` — auth, envelope shape, pagination, CRUD.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_router, ApiState};
use camelmailer_core::MemoryStore;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const GLOBAL_KEY: &str = "global-admin-key";
const DB_KEY: &str = "db-backed-key";

async fn build_app() -> (Router, Arc<ApiState>) {
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::new(store, Some(GLOBAL_KEY.to_string()));
    state
        .store
        .create_admin_api_key("test", DB_KEY)
        .await
        .unwrap();
    (build_router(state.clone()), state)
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(key) = key {
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

// ------------------------------------------------------------------- auth

#[tokio::test]
async fn requests_without_a_key_are_unauthorized() {
    let (app, _) = build_app().await;
    let (status, body) = request(&app, "GET", "/api/v2/admin/organizations", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["status"], "error");
    assert_eq!(body["error"]["code"], "Unauthorized");
    assert_eq!(body["error"]["message"], "Missing X-Admin-API-Key header");
    assert!(body["time"].is_number());
}

#[tokio::test]
async fn requests_with_an_invalid_key_are_unauthorized() {
    let (app, _) = build_app().await;
    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some("wrong"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"]["message"], "Invalid API key");
}

#[tokio::test]
async fn the_global_config_key_authenticates() {
    let (app, _) = build_app().await;
    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
}

#[tokio::test]
async fn database_backed_keys_authenticate() {
    let (app, _) = build_app().await;
    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(DB_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "success");
}

// -------------------------------------------------------------- envelope

#[tokio::test]
async fn success_responses_carry_the_standard_envelope() {
    let (app, _) = build_app().await;
    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["status"], "success");
    assert!(body["time"].is_number());
    assert!(body["data"].is_object());
}

// ------------------------------------------------------------------ CRUD

#[tokio::test]
async fn organizations_can_be_created_shown_listed_and_deleted() {
    let (app, _) = build_app().await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Test Org", "permalink": "test-org" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let organization = &body["data"]["organization"];
    assert_eq!(organization["name"], "Test Org");
    assert_eq!(organization["permalink"], "test-org");
    assert!(organization["uuid"].is_string());

    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/test-org",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["organization"]["name"], "Test Org");

    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["organizations"].as_array().unwrap().len(), 1);
    assert_eq!(body["data"]["pagination"]["total"], 1);

    let (status, body) = request(
        &app,
        "DELETE",
        "/api/v2/admin/organizations/test-org",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["deleted"], true);

    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/test-org",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn creating_an_organization_without_a_name_is_a_parameter_error() {
    let (app, _) = build_app().await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "ParameterMissing");
}

#[tokio::test]
async fn duplicate_permalinks_are_a_validation_error() {
    let (app, _) = build_app().await;
    let payload = json!({ "name": "Test Org", "permalink": "test-org" });
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(payload.clone()),
    )
    .await;
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(payload),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
}

#[tokio::test]
async fn missing_resources_render_not_found() {
    let (app, _) = build_app().await;
    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/nope",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"]["code"], "NotFound");
    assert_eq!(body["error"]["message"], "Resource not found");
}

// ------------------------------------------------------------- pagination

#[tokio::test]
async fn list_endpoints_paginate_and_cap_per_page_at_100() {
    let (app, _) = build_app().await;
    for index in 0..30 {
        request(
            &app,
            "POST",
            "/api/v2/admin/organizations",
            Some(GLOBAL_KEY),
            Some(json!({ "name": format!("Org {index:02}") })),
        )
        .await;
    }

    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["organizations"].as_array().unwrap().len(), 25);
    assert_eq!(body["data"]["pagination"]["page"], 1);
    assert_eq!(body["data"]["pagination"]["per_page"], 25);
    assert_eq!(body["data"]["pagination"]["total"], 30);
    assert_eq!(body["data"]["pagination"]["total_pages"], 2);

    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations?page=2",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["organizations"].as_array().unwrap().len(), 5);

    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations?per_page=1000",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["pagination"]["per_page"], 100);
}

// ---------------------------------------------------------------- servers

#[tokio::test]
async fn servers_are_nested_under_organizations() {
    let (app, _) = build_app().await;
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Test Org", "permalink": "test-org" })),
    )
    .await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/test-org/servers",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Mail Server", "permalink": "mail" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["server"]["mode"], "Live");
    assert_eq!(body["data"]["server"]["suspended"], false);

    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/test-org/servers/mail",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["server"]["name"], "Mail Server");

    // suspend + unsuspend
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/test-org/servers/mail/suspend",
        Some(GLOBAL_KEY),
        Some(json!({ "reason": "abuse" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["server"]["suspended"], true);
    assert_eq!(body["data"]["server"]["suspension_reason"], "abuse");

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/test-org/servers/mail/unsuspend",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["server"]["suspended"], false);

    let (status, _) = request(
        &app,
        "DELETE",
        "/api/v2/admin/organizations/test-org/servers/mail",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/test-org/servers/mail",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn servers_in_a_missing_organization_render_not_found() {
    let (app, _) = build_app().await;
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/nope/servers",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
