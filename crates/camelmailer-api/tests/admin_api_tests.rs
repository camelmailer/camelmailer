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

// ------------------------------------------------- server-scoped resources

async fn build_app_with_server() -> Router {
    let (app, _) = build_app().await;
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Acme", "permalink": "acme" })),
    )
    .await;
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations/acme/servers",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Mail", "permalink": "mail" })),
    )
    .await;
    app
}

const BASE: &str = "/api/v2/admin/organizations/acme/servers/mail";

#[tokio::test]
async fn domains_crud_and_verify() {
    let app = build_app_with_server().await;

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "acme.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["domain"]["name"], "acme.example");
    assert_eq!(body["data"]["domain"]["verified"], false);

    // duplicate name conflicts
    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "acme.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains/acme.example/verify"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["domain"]["verified"], true);

    let (_, body) = request(
        &app,
        "GET",
        &format!("{BASE}/domains"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["domains"].as_array().unwrap().len(), 1);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/domains/acme.example"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        &format!("{BASE}/domains/acme.example"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn credentials_crud_generates_keys_and_holds() {
    let app = build_app_with_server().await;

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/credentials"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "App", "type": "SMTP" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let credential = &body["data"]["credential"];
    assert_eq!(credential["type"], "SMTP");
    assert!(credential["key"].as_str().unwrap().len() >= 24);
    let id = credential["id"].as_u64().unwrap();

    // SMTP-IP requires an explicit CIDR key
    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/credentials"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Relay", "type": "SMTP-IP" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = request(
        &app,
        "PATCH",
        &format!("{BASE}/credentials/{id}"),
        Some(GLOBAL_KEY),
        Some(json!({ "hold": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["credential"]["hold"], true);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/credentials/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn routes_crud_resolves_domains() {
    let app = build_app_with_server().await;
    request(
        &app,
        "POST",
        &format!("{BASE}/domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "acme.example" })),
    )
    .await;

    // unknown domain is a validation error
    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/routes"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "info", "domain": "nope.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/routes"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "info", "domain": "acme.example", "mode": "Accept" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let route = &body["data"]["route"];
    assert_eq!(route["mode"], "Accept");
    assert!(route["token"].as_str().unwrap().len() >= 8);
    let id = route["id"].as_u64().unwrap();

    let (status, body) = request(
        &app,
        "PATCH",
        &format!("{BASE}/routes/{id}"),
        Some(GLOBAL_KEY),
        Some(json!({ "mode": "Reject" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["route"]["mode"], "Reject");

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/routes/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn webhooks_crud_enable_disable() {
    let app = build_app_with_server().await;

    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Hook", "url": "not-a-url" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Hook", "url": "https://hooks.example/cb" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let webhook = &body["data"]["webhook"];
    assert_eq!(webhook["enabled"], true);
    let id = webhook["id"].as_u64().unwrap();

    let (_, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks/{id}/disable"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["webhook"]["enabled"], false);

    let (_, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks/{id}/enable"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["webhook"]["enabled"], true);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/webhooks/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn suppressions_create_list_delete_and_conflict() {
    let app = build_app_with_server().await;

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/suppressions"),
        Some(GLOBAL_KEY),
        Some(json!({ "address": "bounce@example.net", "reason": "hard bounce" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["suppression"]["type"], "recipient");

    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/suppressions"),
        Some(GLOBAL_KEY),
        Some(json!({ "address": "bounce@example.net" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, body) = request(
        &app,
        "GET",
        &format!("{BASE}/suppressions"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["suppressions"].as_array().unwrap().len(), 1);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/suppressions/bounce@example.net"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/suppressions/bounce@example.net"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn users_crud_with_unique_email() {
    let (app, _) = build_app().await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/users",
        Some(GLOBAL_KEY),
        Some(json!({ "email_address": "admin@example.com", "first_name": "Ada", "last_name": "Admin", "admin": true })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = body["data"]["user"]["id"].as_u64().unwrap();
    assert_eq!(body["data"]["user"]["admin"], true);

    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/admin/users",
        Some(GLOBAL_KEY),
        Some(json!({ "email_address": "admin@example.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/admin/users",
        Some(GLOBAL_KEY),
        Some(json!({ "email_address": "no-at-sign" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (_, body) = request(
        &app,
        "PATCH",
        &format!("/api/v2/admin/users/{id}"),
        Some(GLOBAL_KEY),
        Some(json!({ "admin": false })),
    )
    .await;
    assert_eq!(body["data"]["user"]["admin"], false);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/api/v2/admin/users/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn ip_pools_and_nested_addresses() {
    let (app, _) = build_app().await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/ip_pools",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Pool 1", "default": true })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let pool_id = body["data"]["ip_pool"]["id"].as_u64().unwrap();

    let (status, _) = request(
        &app,
        "POST",
        &format!("/api/v2/admin/ip_pools/{pool_id}/ip_addresses"),
        Some(GLOBAL_KEY),
        Some(json!({ "ipv4": "not-an-ip", "hostname": "mx.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    let (status, body) = request(
        &app,
        "POST",
        &format!("/api/v2/admin/ip_pools/{pool_id}/ip_addresses"),
        Some(GLOBAL_KEY),
        Some(json!({ "ipv4": "192.0.2.10", "hostname": "mx.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let address_id = body["data"]["ip_address"]["id"].as_u64().unwrap();
    assert_eq!(body["data"]["ip_address"]["priority"], 100);

    let (_, body) = request(
        &app,
        "GET",
        &format!("/api/v2/admin/ip_pools/{pool_id}/ip_addresses"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["ip_addresses"].as_array().unwrap().len(), 1);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/api/v2/admin/ip_pools/{pool_id}/ip_addresses/{address_id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/api/v2/admin/ip_pools/{pool_id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ------------------------------------------------ P0: server config + keys

#[tokio::test]
async fn server_update_persists_config_fields() {
    let app = build_app_with_server().await;

    let (status, body) = request(
        &app,
        "PATCH",
        "/api/v2/admin/organizations/acme/servers/mail",
        Some(GLOBAL_KEY),
        Some(json!({
            "track_opens": true,
            "track_clicks": true,
            "spam_threshold": 5.5,
            "color": "#ff0000",
            "inbound_domain": "in.acme.example",
            "mode": "Development"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let server = &body["data"]["server"];
    assert_eq!(server["track_opens"], true);
    assert_eq!(server["track_clicks"], true);
    assert_eq!(server["spam_threshold"], 5.5);
    assert_eq!(server["color"], "#ff0000");
    assert_eq!(server["inbound_domain"], "in.acme.example");
    assert_eq!(server["mode"], "Development");

    // invalid mode → 422
    let (status, _) = request(
        &app,
        "PATCH",
        "/api/v2/admin/organizations/acme/servers/mail",
        Some(GLOBAL_KEY),
        Some(json!({ "mode": "Nonsense" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn admin_api_keys_create_list_delete() {
    let (app, _) = build_app().await;

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/admin_api_keys",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "ci-key" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let created = &body["data"]["admin_api_key"];
    assert_eq!(created["name"], "ci-key");
    let full_key = created["key"].as_str().unwrap().to_string();
    assert!(full_key.len() >= 24);
    assert_eq!(created["key_prefix"], &full_key[..6]);
    let id = created["id"].as_u64().unwrap();

    // the new key actually authenticates
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(&full_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // list shows only the prefix, never the secret (build_app pre-seeds one key)
    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/admin_api_keys",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    let keys = body["data"]["admin_api_keys"].as_array().unwrap();
    let ci = keys.iter().find(|k| k["name"] == "ci-key").unwrap();
    assert_eq!(ci["key_prefix"], &full_key[..6]);
    assert!(keys.iter().all(|k| k.get("key").is_none()));

    let (status, _) = request(
        &app,
        "DELETE",
        &format!("/api/v2/admin/admin_api_keys/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // deleted key no longer authenticates
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(&full_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn server_ip_pool_assignment() {
    let app = build_app_with_server().await;
    let (_, body) = request(
        &app,
        "POST",
        "/api/v2/admin/ip_pools",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Pool" })),
    )
    .await;
    let pool_id = body["data"]["ip_pool"]["id"].as_u64().unwrap();

    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/acme/servers/mail/ip_pool",
        Some(GLOBAL_KEY),
        Some(json!({ "ip_pool_id": pool_id })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["server"]["ip_pool_id"], pool_id);
}

#[tokio::test]
async fn health_is_public_and_reports_the_version() {
    let (app, _) = build_app().await;
    // no API key required — this is the container/LB liveness probe
    let (status, body) = request(&app, "GET", "/health", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
}
