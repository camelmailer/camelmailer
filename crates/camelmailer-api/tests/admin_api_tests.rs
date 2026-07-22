//! Port of the Admin API v2 conventions covered by
//! `spec/apis/admin_api/` — auth, envelope shape, pagination, CRUD.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_router, ApiState};
use camelmailer_core::{MemoryStore, StaticDnsResolver};
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
    assert_eq!(
        body["error"]["message"],
        "Missing X-Admin-API-Key header or Authorization: Bearer session token"
    );
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
    build_app_with_resolver().await.0
}

/// Like [`build_app_with_server`] but also returns the injected mock DNS
/// resolver, for the domain-verification tests.
async fn build_app_with_resolver() -> (Router, Arc<StaticDnsResolver>) {
    let resolver = Arc::new(StaticDnsResolver::new());
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::new_with_resolver(store, Some(GLOBAL_KEY.to_string()), resolver.clone());
    let app = build_router(state);
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
    (app, resolver)
}

const BASE: &str = "/api/v2/admin/organizations/acme/servers/mail";

#[tokio::test]
async fn domains_crud_returns_dns_records_and_verifies_via_dns() {
    let (app, resolver) = build_app_with_resolver().await;

    // create → the three records to publish, never the private key
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "acme.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let domain = &body["data"]["domain"];
    assert_eq!(domain["name"], "acme.example");
    assert_eq!(domain["verified"], false);
    assert_eq!(
        domain["verification_record"]["name"],
        "_camelmailer-challenge.acme.example"
    );
    assert_eq!(domain["verification_record"]["type"], "TXT");
    let challenge = domain["verification_record"]["value"].as_str().unwrap();
    assert!(challenge.starts_with("camelmailer-verification="));
    assert_eq!(
        domain["dkim_record"]["name"],
        "postal._domainkey.acme.example"
    );
    assert_eq!(domain["dkim_record"]["type"], "TXT");
    assert!(domain["dkim_record"]["value"]
        .as_str()
        .unwrap()
        .starts_with("v=DKIM1; k=rsa; p="));
    assert_eq!(domain["spf_record"]["name"], "acme.example");
    assert_eq!(domain["spf_record"]["type"], "TXT");
    assert_eq!(
        domain["spf_record"]["value"],
        "v=spf1 include:spf.postal.example.com ~all"
    );
    assert!(
        !body.to_string().contains("PRIVATE KEY"),
        "the DKIM private key must never leave the API"
    );
    let challenge = challenge.to_string();

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

    // verify without the TXT record → 422 naming the record to publish
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains/acme.example/verify"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("_camelmailer-challenge.acme.example"));
    assert!(message.contains(&challenge));

    // a DNS failure is also a 422, not a success
    resolver.fail_with("SERVFAIL");
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains/acme.example/verify"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");

    // publish the token → verified
    let (_, body) = request(
        &app,
        "GET",
        &format!("{BASE}/domains/acme.example"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    // the token is stable across reads
    assert_eq!(
        body["data"]["domain"]["verification_record"]["value"],
        challenge
    );

    resolver.add_txt("_camelmailer-challenge.acme.example", &challenge);
    resolver.clear_error();
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains/acme.example/verify"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "verify failed: {body}");
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
    assert_eq!(body["data"]["domains"][0]["verified"], true);

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
async fn force_verify_with_the_machine_key_skips_the_dns_check() {
    let (app, _resolver) = build_app_with_resolver().await;
    request(
        &app,
        "POST",
        &format!("{BASE}/domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "forced.example" })),
    )
    .await;

    // no TXT record published anywhere — force skips the lookup
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains/forced.example/verify"),
        Some(GLOBAL_KEY),
        Some(json!({ "force": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["domain"]["verified"], true);
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
    // never used yet
    assert!(credential["last_used_at"].is_null());
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
            "broadcast_physical_address": "Acme Inc, 1 Main St",
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
    assert_eq!(server["broadcast_physical_address"], "Acme Inc, 1 Main St");
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

// ------------------------------------- webhook events + custom headers

#[tokio::test]
async fn webhooks_accept_subscribed_events_and_custom_headers() {
    let app = build_app_with_server().await;

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks"),
        Some(GLOBAL_KEY),
        Some(json!({
            "name": "Filtered",
            "url": "https://hooks.example/cb",
            "events": ["MessageSent", "MessageDeliveryFailed"],
            "headers": { "Authorization": "Bearer hunter2", "X-Custom": "1" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let webhook = &body["data"]["webhook"];
    assert_eq!(
        webhook["events"],
        json!(["MessageSent", "MessageDeliveryFailed"])
    );
    assert_eq!(webhook["headers"]["Authorization"], "Bearer hunter2");
    assert_eq!(webhook["headers"]["X-Custom"], "1");
    // a non-empty subscription is not "all events"
    assert_eq!(webhook["all_events"], false);
    let id = webhook["id"].as_u64().unwrap();

    // GET returns both fields
    let (status, body) = request(
        &app,
        "GET",
        &format!("{BASE}/webhooks/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["webhook"]["events"],
        json!(["MessageSent", "MessageDeliveryFailed"])
    );
    assert_eq!(
        body["data"]["webhook"]["headers"]["Authorization"],
        "Bearer hunter2"
    );

    // PATCH updates events and headers; clearing events = all events again
    let (status, body) = request(
        &app,
        "PATCH",
        &format!("{BASE}/webhooks/{id}"),
        Some(GLOBAL_KEY),
        Some(json!({ "events": [], "headers": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["webhook"]["events"], json!([]));
    assert_eq!(body["data"]["webhook"]["headers"], json!({}));
    assert_eq!(body["data"]["webhook"]["all_events"], true);
}

#[tokio::test]
async fn webhooks_default_to_all_events_without_headers() {
    let app = build_app_with_server().await;
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Hook", "url": "https://hooks.example/cb" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["data"]["webhook"]["events"], json!([]));
    assert_eq!(body["data"]["webhook"]["headers"], json!({}));
    assert_eq!(body["data"]["webhook"]["all_events"], true);
}

#[tokio::test]
async fn webhooks_reject_unknown_event_names_listing_the_valid_ones() {
    let app = build_app_with_server().await;
    for (method, path, body) in [(
        "POST",
        format!("{BASE}/webhooks"),
        json!({ "name": "Hook", "url": "https://hooks.example/cb", "events": ["MessageOpened"] }),
    )] {
        let (status, response) = request(&app, method, &path, Some(GLOBAL_KEY), Some(body)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(response["error"]["code"], "ValidationError");
        let message = response["error"]["message"].as_str().unwrap();
        assert!(message.contains("\"MessageOpened\""), "{message}");
        for valid in [
            "MessageSent",
            "MessageDelayed",
            "MessageDeliveryFailed",
            "MessageHeld",
        ] {
            assert!(message.contains(valid), "{message} should list {valid}");
        }
    }

    // the same validation applies on update
    let (_, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Hook", "url": "https://hooks.example/cb" })),
    )
    .await;
    let id = body["data"]["webhook"]["id"].as_u64().unwrap();
    let (status, response) = request(
        &app,
        "PATCH",
        &format!("{BASE}/webhooks/{id}"),
        Some(GLOBAL_KEY),
        Some(json!({ "events": ["Nonsense"] })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(response["error"]["code"], "ValidationError");
    assert!(response["error"]["message"]
        .as_str()
        .unwrap()
        .contains("MessageHeld"));
}

#[tokio::test]
async fn webhooks_reject_invalid_header_names_without_echoing_values() {
    let app = build_app_with_server().await;
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks"),
        Some(GLOBAL_KEY),
        Some(json!({
            "name": "Hook",
            "url": "https://hooks.example/cb",
            "headers": { "bad header name": "top-secret-value" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("bad header name"), "{message}");
    // the header VALUE must never appear in an error (or a log)
    assert!(!message.contains("top-secret-value"), "{message}");

    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/webhooks"),
        Some(GLOBAL_KEY),
        Some(json!({
            "name": "Hook",
            "url": "https://hooks.example/cb",
            "headers": { "X-Ok": "bad\nvalue-top-secret" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("X-Ok"), "{message}");
    assert!(!message.contains("top-secret"), "{message}");
}

// -------------------------------------------------- sender addresses

#[tokio::test]
async fn sender_addresses_create_list_delete() {
    let app = build_app_with_server().await;

    // missing / invalid email
    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/sender_addresses"),
        Some(GLOBAL_KEY),
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/sender_addresses"),
        Some(GLOBAL_KEY),
        Some(json!({ "email": "not-an-email" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");

    // create: pending, and (app_mail disabled) the one-time token is returned
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/sender_addresses"),
        Some(GLOBAL_KEY),
        Some(json!({ "email": "Solo@External.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let address = &body["data"]["sender_address"];
    assert_eq!(address["email_address"], "solo@external.example");
    assert_eq!(address["verified"], false);
    assert_eq!(address["status"], "pending");
    assert!(body["data"]["verification_token"].is_string());
    let id = address["id"].as_u64().unwrap();

    // duplicates conflict (case-insensitive)
    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/sender_addresses"),
        Some(GLOBAL_KEY),
        Some(json!({ "email": "solo@external.example" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // list contains it — without any token material
    let (status, body) = request(
        &app,
        "GET",
        &format!("{BASE}/sender_addresses"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let listed = body["data"]["sender_addresses"].as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].get("verification_token").is_none());
    assert!(listed[0].get("verification_token_hash").is_none());

    // delete
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/sender_addresses/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/sender_addresses/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn credential_listings_carry_last_used_at_after_a_use() {
    use camelmailer_core::AdminStore;
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::new(store.clone(), Some(GLOBAL_KEY.to_string()));
    let app = build_router(state);
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
    let (_, body) = request(
        &app,
        "POST",
        "/api/v2/admin/organizations/acme/servers/mail/credentials",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "App", "type": "API" })),
    )
    .await;
    let key = body["data"]["credential"]["key"]
        .as_str()
        .unwrap()
        .to_string();

    // never used: the listing shows null
    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/acme/servers/mail/credentials",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert!(body["data"]["credentials"][0]["last_used_at"].is_null());

    // a per-server API authentication records the use ...
    store.server_for_api_token(&key).await.unwrap().unwrap();

    // ... and the management listing now carries the timestamp
    let (_, body) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/acme/servers/mail/credentials",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert!(body["data"]["credentials"][0]["last_used_at"].is_string());
}

// -------------------------------------------------- global server resolver

#[tokio::test]
async fn servers_find_resolves_a_permalink_across_organizations() {
    let app = build_app_with_server().await;

    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/servers/find/mail",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["organization"]["permalink"], "acme");
    assert_eq!(body["data"]["server"]["permalink"], "mail");

    // unknown permalinks are a 404
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/servers/find/nope",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // the same permalink in a second organization makes the lookup ambiguous
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Beta", "permalink": "beta" })),
    )
    .await;
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations/beta/servers",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Mail", "permalink": "mail" })),
    )
    .await;
    let (status, body) = request(
        &app,
        "GET",
        "/api/v2/admin/servers/find/mail",
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");
}

// ---------------------------------------------------- scoped admin API keys

/// Create a scoped key via the API and return its plaintext.
async fn create_scoped_key(app: &Router, body: Value) -> String {
    let (status, body) = request(
        app,
        "POST",
        "/api/v2/admin/admin_api_keys",
        Some(GLOBAL_KEY),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    body["data"]["admin_api_key"]["key"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn scoped_admin_api_keys_only_reach_their_subtree() {
    let app = build_app_with_server().await;
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations/acme/servers",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Second", "permalink": "second" })),
    )
    .await;
    request(
        &app,
        "POST",
        "/api/v2/admin/organizations",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Beta", "permalink": "beta" })),
    )
    .await;

    let org_key =
        create_scoped_key(&app, json!({ "name": "acme-key", "organization": "acme" })).await;
    let server_key = create_scoped_key(
        &app,
        json!({ "name": "mail-key", "organization": "acme", "server": "mail" }),
    )
    .await;

    // the org-scoped key reaches its organization and every server in it
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/acme",
        Some(&org_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/acme/servers/second",
        Some(&org_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // …but never a foreign organization or global resources — both 404-shaped
    // or denied, without leaking existence
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/beta",
        Some(&org_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(&app, "GET", "/api/v2/admin/users", Some(&org_key), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations",
        Some(&org_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/servers/find/mail",
        Some(&org_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // the server-scoped key reaches only its server's subtree
    let (status, _) = request(
        &app,
        "GET",
        &format!("{BASE}/credentials"),
        Some(&server_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        &format!("{BASE}/domains"),
        Some(&server_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/acme/servers/second/credentials",
        Some(&server_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = request(
        &app,
        "GET",
        "/api/v2/admin/organizations/acme",
        Some(&server_key),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // a scoped machine key still counts as a machine key: force-verify works
    request(
        &app,
        "POST",
        &format!("{BASE}/domains"),
        Some(&server_key),
        Some(json!({ "name": "acme.com" })),
    )
    .await;
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/domains/acme.com/verify"),
        Some(&server_key),
        Some(json!({ "force": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["domain"]["verified"], true);
}

#[tokio::test]
async fn scoped_key_creation_validates_its_scope() {
    let app = build_app_with_server().await;

    // a server scope needs its organization
    let (status, body) = request(
        &app,
        "POST",
        "/api/v2/admin/admin_api_keys",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "x", "server": "mail" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");

    // unknown organizations and servers are validation errors
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/admin/admin_api_keys",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "x", "organization": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = request(
        &app,
        "POST",
        "/api/v2/admin/admin_api_keys",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "x", "organization": "acme", "server": "nope" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // the scope is visible on the record (ids), the secret only at creation
    let (_, body) = request(
        &app,
        "POST",
        "/api/v2/admin/admin_api_keys",
        Some(GLOBAL_KEY),
        Some(json!({ "name": "scoped", "organization": "acme", "server": "mail" })),
    )
    .await;
    let created = &body["data"]["admin_api_key"];
    assert!(created["organization_id"].is_u64());
    assert!(created["server_id"].is_u64());
}

// ------------------------------------------------------------ track domains

#[tokio::test]
async fn track_domains_crud_and_cname_verification() {
    let (app, resolver) = build_app_with_resolver().await;

    // create returns the CNAME record to publish
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/track_domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "Track.Acme.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let created = &body["data"]["track_domain"];
    assert_eq!(created["name"], "track.acme.com");
    assert_eq!(created["verified"], false);
    assert_eq!(created["cname_record"]["type"], "CNAME");
    let id = created["id"].as_u64().unwrap();

    // duplicates conflict; a bare word is not a hostname
    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/track_domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "track.acme.com" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, _) = request(
        &app,
        "POST",
        &format!("{BASE}/track_domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "track" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // verify fails while the CNAME is missing and names the record to publish
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/track_domains/{id}/verify"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"]["message"].as_str().unwrap().contains("CNAME"));

    // with the CNAME pointing at the installation (the default config's
    // web hostname; a trailing dot is tolerated) it verifies
    resolver.add_cname("track.acme.com", "postal.example.com.");
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/track_domains/{id}/verify"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["track_domain"]["verified"], true);

    // list + show
    let (_, body) = request(
        &app,
        "GET",
        &format!("{BASE}/track_domains"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(body["data"]["track_domains"][0]["verified"], true);
    let (status, _) = request(
        &app,
        "GET",
        &format!("{BASE}/track_domains/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // delete
    let (status, _) = request(
        &app,
        "DELETE",
        &format!("{BASE}/track_domains/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request(
        &app,
        "GET",
        &format!("{BASE}/track_domains/{id}"),
        Some(GLOBAL_KEY),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn track_domain_force_verify_is_machine_key_only_wire_shape() {
    let (app, _) = build_app_with_resolver().await;
    let (_, body) = request(
        &app,
        "POST",
        &format!("{BASE}/track_domains"),
        Some(GLOBAL_KEY),
        Some(json!({ "name": "t.acme.com" })),
    )
    .await;
    let id = body["data"]["track_domain"]["id"].as_u64().unwrap();

    // machine key force-verifies without any DNS record
    let (status, body) = request(
        &app,
        "POST",
        &format!("{BASE}/track_domains/{id}/verify"),
        Some(GLOBAL_KEY),
        Some(json!({ "force": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["track_domain"]["verified"], true);
}
