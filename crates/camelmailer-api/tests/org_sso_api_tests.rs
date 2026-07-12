//! Router tests for the tenant SSO configuration API
//! (`/api/v2/admin/organizations/{permalink}/sso/…`): domain lifecycle,
//! connection CRUD, secret masking, and cross-organization isolation.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_router, ApiState};
use camelmailer_core::{MemoryStore, StaticDnsResolver};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const KEY: &str = "global-admin-key";
const MASK: &str = "••••••••";

async fn build_app() -> (Router, Arc<ApiState>) {
    let store = Arc::new(MemoryStore::new());
    let resolver = Arc::new(StaticDnsResolver::new());
    let state = ApiState::new_with_resolver(store.clone(), Some(KEY.to_string()), resolver)
        .with_org_sso_store(store.clone());
    (build_router(state.clone()), state)
}

async fn request(
    app: &Router,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("X-Admin-API-Key", KEY);
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

async fn create_org(app: &Router, name: &str) -> (String, u64) {
    let (status, body) = request(
        app,
        "POST",
        "/api/v2/admin/organizations",
        Some(json!({ "name": name })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let org = &body["data"]["organization"];
    (
        org["permalink"].as_str().unwrap().to_string(),
        org["id"].as_u64().unwrap(),
    )
}

#[tokio::test]
async fn domain_lifecycle_verifies_and_lists() {
    let (app, _) = build_app().await;
    let (org, _) = create_org(&app, "Acme").await;
    let base = format!("/api/v2/admin/organizations/{org}/sso/domains");

    // create: unverified, returns the DNS challenge record
    let (status, body) = request(&app, "POST", &base, Some(json!({ "domain": "Acme.COM" }))).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let domain = &body["data"]["domain"];
    assert_eq!(domain["domain"], "acme.com");
    assert_eq!(domain["verified"], false);
    assert_eq!(
        domain["dns_record"]["name"],
        "_camelmailer-challenge.acme.com"
    );
    let id = domain["id"].as_u64().unwrap();

    // non-force verify fails without the TXT record (empty static resolver)
    let (status, body) = request(&app, "POST", &format!("{base}/{id}/verify"), None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");

    // forced verify with the machine key succeeds
    let (status, body) = request(
        &app,
        "POST",
        &format!("{base}/{id}/verify"),
        Some(json!({ "force": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["domain"]["verified"], true);

    let (status, body) = request(&app, "GET", &base, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["domains"].as_array().unwrap().len(), 1);

    let (status, _) = request(&app, "DELETE", &format!("{base}/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = request(&app, "GET", &base, None).await;
    assert!(body["data"]["domains"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn connection_secrets_are_masked_on_read_and_preserved_on_masked_update() {
    let (app, state) = build_app().await;
    let (org, org_id) = create_org(&app, "Acme").await;
    let base = format!("/api/v2/admin/organizations/{org}/sso/connections");

    // create with a real secret
    let (status, body) = request(
        &app,
        "POST",
        &base,
        Some(json!({
            "kind": "oidc",
            "name": "Acme Okta",
            "config": { "issuer": "https://acme.okta.com", "client_id": "abc", "client_secret": "supersecret" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["data"]["connection"]["id"].as_u64().unwrap();
    // the response never carries the real secret
    assert_eq!(body["data"]["connection"]["config"]["client_secret"], MASK);
    // but the store holds it
    let raw = state
        .org_sso_store
        .as_ref()
        .unwrap()
        .list_org_sso_connections(org_id)
        .await
        .unwrap();
    assert_eq!(raw[0].config["client_secret"], "supersecret");

    // update echoing the mask back keeps the stored secret; name changes
    let (status, body) = request(
        &app,
        "PATCH",
        &format!("{base}/{id}"),
        Some(json!({
            "name": "Renamed",
            "config": { "issuer": "https://acme.okta.com", "client_id": "abc", "client_secret": MASK },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["connection"]["name"], "Renamed");
    let raw = state
        .org_sso_store
        .as_ref()
        .unwrap()
        .org_sso_connection(id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw.config["client_secret"], "supersecret");

    // update with a genuinely new secret replaces it
    let (status, _) = request(
        &app,
        "PATCH",
        &format!("{base}/{id}"),
        Some(json!({
            "config": { "issuer": "https://acme.okta.com", "client_id": "abc", "client_secret": "rotated" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let raw = state
        .org_sso_store
        .as_ref()
        .unwrap()
        .org_sso_connection(id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(raw.config["client_secret"], "rotated");
}

#[tokio::test]
async fn connections_are_isolated_between_organizations() {
    let (app, _) = build_app().await;
    let (org_a, _) = create_org(&app, "Acme").await;
    let (org_b, _) = create_org(&app, "Beta").await;

    let (status, body) = request(
        &app,
        "POST",
        &format!("/api/v2/admin/organizations/{org_a}/sso/connections"),
        Some(json!({
            "kind": "google",
            "name": "Google",
            "config": { "client_id": "gid", "client_secret": "gsecret" },
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["data"]["connection"]["id"].as_u64().unwrap();

    // org B cannot see, edit or delete org A's connection
    for method in ["GET", "PATCH", "DELETE"] {
        let (status, _) = request(
            &app,
            method,
            &format!("/api/v2/admin/organizations/{org_b}/sso/connections/{id}"),
            if method == "PATCH" {
                Some(json!({ "name": "x" }))
            } else {
                None
            },
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} should be 404");
    }
}

#[tokio::test]
async fn creating_a_connection_validates_kind_and_required_fields() {
    let (app, _) = build_app().await;
    let (org, _) = create_org(&app, "Acme").await;
    let base = format!("/api/v2/admin/organizations/{org}/sso/connections");

    let (status, _) = request(
        &app,
        "POST",
        &base,
        Some(json!({ "kind": "carrier-pigeon", "name": "Nope", "config": {} })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);

    // oidc without an issuer is rejected
    let (status, _) = request(
        &app,
        "POST",
        &base,
        Some(json!({ "kind": "oidc", "name": "Half", "config": { "client_id": "abc" } })),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}
