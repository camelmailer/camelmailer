//! SCIM 2.0 provisioning (`/scim/v2`): bearer-token auth, the discovery
//! endpoints, the Users CRUD cycle, filtering, and the interplay with
//! login (`active: false` blocks `/api/v2/auth/login`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, build_scim_router, ApiState};
use camelmailer_core::{AuthStore, MemoryStore};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const TOKEN: &str = "scim-secret-token";

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
}

async fn harness(mutate: impl FnOnce(&mut camelmailer_config::Config)) -> Harness {
    let mut config = camelmailer_config::Config::default();
    config.scim.enabled = true;
    config.scim.bearer_token = Some(TOKEN.into());
    mutate(&mut config);
    let store = Arc::new(MemoryStore::new());
    let state = ApiState::full(store.clone(), None, Some(store.clone()), None, config);
    let app = build_scim_router(state.clone()).merge(build_auth_router(state));
    Harness { app, store }
}

impl Harness {
    async fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value, Option<String>) {
        let mut builder = Request::builder().method(method).uri(path);
        if let Some(token) = token {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/scim+json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, json, content_type)
    }

    async fn scim(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value, Option<String>) {
        self.request(method, path, Some(TOKEN), body).await
    }

    async fn login(&self, email: &str, password: &str) -> (StatusCode, Value) {
        let (status, body, _) = self
            .request(
                "POST",
                "/api/v2/auth/login",
                None,
                Some(json!({ "email_address": email, "password": password })),
            )
            .await;
        (status, body)
    }
}

// ---------------------------------------------------------------- auth

#[tokio::test]
async fn requests_without_a_valid_token_are_rejected() {
    let h = harness(|_| {}).await;
    // missing token
    let (status, body, content_type) = h.request("GET", "/scim/v2/Users", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        body["schemas"][0],
        "urn:ietf:params:scim:api:messages:2.0:Error"
    );
    assert_eq!(body["status"], "401");
    assert_eq!(content_type.as_deref(), Some("application/scim+json"));
    // wrong token
    let (status, _, _) = h
        .request("GET", "/scim/v2/Users", Some("wrong-token"), None)
        .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn scim_answers_404_when_disabled() {
    let h = harness(|config| {
        config.scim.enabled = false;
    })
    .await;
    let (status, body, _) = h.scim("GET", "/scim/v2/Users", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        body["schemas"][0],
        "urn:ietf:params:scim:api:messages:2.0:Error"
    );
}

// ----------------------------------------------------------- discovery

#[tokio::test]
async fn discovery_endpoints_are_served() {
    let h = harness(|_| {}).await;

    let (status, body, content_type) = h.scim("GET", "/scim/v2/ServiceProviderConfig", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(content_type.as_deref(), Some("application/scim+json"));
    assert_eq!(body["patch"]["supported"], true);
    assert_eq!(body["bulk"]["supported"], false);
    assert_eq!(body["filter"]["supported"], true);

    let (status, body, _) = h.scim("GET", "/scim/v2/ResourceTypes", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["Resources"][0]["id"], "User");
    assert_eq!(body["Resources"][0]["endpoint"], "/Users");

    let (status, body, _) = h.scim("GET", "/scim/v2/Schemas", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["Resources"][0]["id"],
        "urn:ietf:params:scim:schemas:core:2.0:User"
    );
}

// ------------------------------------------------------------ the CRUD

#[tokio::test]
async fn the_full_user_crud_cycle() {
    let h = harness(|_| {}).await;

    // Create
    let (status, created, _) = h
        .scim(
            "POST",
            "/scim/v2/Users",
            Some(json!({
                "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
                "userName": "grace@corp.example",
                "name": { "givenName": "Grace", "familyName": "Hopper" },
                "active": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    assert_eq!(created["userName"], "grace@corp.example");
    assert_eq!(created["name"]["givenName"], "Grace");
    assert_eq!(created["active"], true);
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["meta"]["location"], format!("/scim/v2/Users/{id}"));

    // Read
    let (status, fetched, _) = h.scim("GET", &format!("/scim/v2/Users/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["userName"], "grace@corp.example");
    assert_eq!(fetched["emails"][0]["value"], "grace@corp.example");

    // List
    let (status, list, _) = h.scim("GET", "/scim/v2/Users", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        list["schemas"][0],
        "urn:ietf:params:scim:api:messages:2.0:ListResponse"
    );
    assert_eq!(list["totalResults"], 1);
    assert_eq!(list["Resources"][0]["id"], id.as_str());

    // Replace (PUT)
    let (status, replaced, _) = h
        .scim(
            "PUT",
            &format!("/scim/v2/Users/{id}"),
            Some(json!({
                "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
                "userName": "grace.hopper@corp.example",
                "name": { "givenName": "Grace", "familyName": "Hopper-Murray" },
                "active": true,
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{replaced}");
    assert_eq!(replaced["userName"], "grace.hopper@corp.example");
    assert_eq!(replaced["name"]["familyName"], "Hopper-Murray");

    // Patch a name field
    let (status, patched, _) = h
        .scim(
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            Some(json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [
                    { "op": "replace", "path": "name.givenName", "value": "Amazing Grace" }
                ],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["name"]["givenName"], "Amazing Grace");

    // Delete → deactivates, does not destroy
    let (status, _, _) = h
        .scim("DELETE", &format!("/scim/v2/Users/{id}"), None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, after, _) = h.scim("GET", &format!("/scim/v2/Users/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(after["active"], false);
    let users = camelmailer_core::AdminStore::list_users(h.store.as_ref())
        .await
        .unwrap();
    assert_eq!(users.len(), 1, "DELETE must not hard-delete the account");
}

#[tokio::test]
async fn creating_a_duplicate_user_name_conflicts() {
    let h = harness(|_| {}).await;
    let body = json!({
        "schemas": ["urn:ietf:params:scim:schemas:core:2.0:User"],
        "userName": "ada@corp.example",
    });
    let (status, _, _) = h.scim("POST", "/scim/v2/Users", Some(body.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, error, _) = h.scim("POST", "/scim/v2/Users", Some(body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        error["schemas"][0],
        "urn:ietf:params:scim:api:messages:2.0:Error"
    );
    assert_eq!(error["status"], "409");
    assert_eq!(error["scimType"], "uniqueness");
}

#[tokio::test]
async fn user_name_must_be_an_email_address() {
    let h = harness(|_| {}).await;
    let (status, error, _) = h
        .scim(
            "POST",
            "/scim/v2/Users",
            Some(json!({ "userName": "not-an-email" })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["scimType"], "invalidValue");
}

#[tokio::test]
async fn unknown_users_yield_scim_404s() {
    let h = harness(|_| {}).await;
    for (method, body) in [
        ("GET", None),
        ("PUT", Some(json!({ "userName": "x@y.z" }))),
        (
            "PATCH",
            Some(json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{ "op": "replace", "path": "active", "value": false }],
            })),
        ),
        ("DELETE", None),
    ] {
        let (status, error, _) = h.scim(method, "/scim/v2/Users/999999", body).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method}");
        assert_eq!(error["status"], "404", "{method}");
    }
}

// ------------------------------------------------------------ filtering

#[tokio::test]
async fn users_can_be_filtered_by_user_name() {
    let h = harness(|_| {}).await;
    for email in ["ada@corp.example", "grace@corp.example"] {
        let (status, _, _) = h
            .scim("POST", "/scim/v2/Users", Some(json!({ "userName": email })))
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }

    // IdPs quote and URL-encode: filter=userName eq "ada@corp.example"
    let (status, list, _) = h
        .scim(
            "GET",
            "/scim/v2/Users?filter=userName%20eq%20%22ADA@corp.example%22",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["totalResults"], 1);
    assert_eq!(list["Resources"][0]["userName"], "ada@corp.example");

    // no match → empty list, not an error
    let (status, list, _) = h
        .scim(
            "GET",
            "/scim/v2/Users?filter=userName%20eq%20%22nobody@corp.example%22",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list["totalResults"], 0);

    // unsupported filters are a SCIM 400
    let (status, error, _) = h
        .scim("GET", "/scim/v2/Users?filter=title%20co%20%22boss%22", None)
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["scimType"], "invalidFilter");
}

#[tokio::test]
async fn user_lists_paginate_with_start_index_and_count() {
    let h = harness(|_| {}).await;
    for n in 1..=5 {
        let (status, _, _) = h
            .scim(
                "POST",
                "/scim/v2/Users",
                Some(json!({ "userName": format!("user{n}@corp.example") })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED);
    }
    let (status, page, _) = h
        .scim("GET", "/scim/v2/Users?startIndex=2&count=2", None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(page["totalResults"], 5);
    assert_eq!(page["startIndex"], 2);
    assert_eq!(page["itemsPerPage"], 2);
    assert_eq!(page["Resources"][0]["userName"], "user2@corp.example");
    assert_eq!(page["Resources"][1]["userName"], "user3@corp.example");
}

// ----------------------------------------------- deactivation and login

#[tokio::test]
async fn patch_active_false_blocks_login_and_revokes_sessions() {
    let h = harness(|_| {}).await;

    // Provision via SCIM and give the account a password.
    let (status, created, _) = h
        .scim(
            "POST",
            "/scim/v2/Users",
            Some(json!({
                "userName": "ada@corp.example",
                "name": { "givenName": "Ada", "familyName": "Lovelace" },
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let id = created["id"].as_str().unwrap().to_string();
    let user_id: camelmailer_core::Id = id.parse().unwrap();
    let digest = camelmailer_core::auth::hash_password("s3cret-password").unwrap();
    h.store.set_password_digest(user_id, &digest).await.unwrap();

    // Login works while active.
    let (status, body) = h.login("ada@corp.example", "s3cret-password").await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let session_token = body["data"]["session_token"].as_str().unwrap().to_string();

    // Deactivate via PATCH (Azure-AD-style capitalized string value).
    let (status, patched, _) = h
        .scim(
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            Some(json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{ "op": "Replace", "path": "active", "value": "False" }],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["active"], false);

    // Login is now blocked with a stable error code …
    let (status, body) = h.login("ada@corp.example", "s3cret-password").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"]["code"], "AccountDisabled");

    // … the old session was revoked …
    let (status, _, _) = h.request("GET", "/api/v2/auth/me", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let response = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/v2/auth/me")
                .header("authorization", format!("Bearer {session_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    // … and a password reset cannot resurrect the account.
    let (status, _, _) = h
        .request(
            "POST",
            "/api/v2/auth/password-reset",
            None,
            Some(json!({ "email_address": "ada@corp.example" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK); // deliberately indistinguishable

    // Reactivating restores login.
    let (status, patched, _) = h
        .scim(
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            Some(json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{ "op": "replace", "value": { "active": true } }],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{patched}");
    assert_eq!(patched["active"], true);
    let (status, _) = h.login("ada@corp.example", "s3cret-password").await;
    assert_eq!(status, StatusCode::CREATED);

    // The audit log carries the SCIM lifecycle.
    let events: Vec<String> = h
        .store
        .list_auth_events(20)
        .await
        .unwrap()
        .iter()
        .map(|event| event.event.clone())
        .collect();
    assert!(events.contains(&"scim.provision".to_string()));
    assert!(events.contains(&"scim.deactivate".to_string()));
    assert!(events.contains(&"scim.reactivate".to_string()));
    assert!(events.contains(&"login.disabled".to_string()));
}

#[tokio::test]
async fn creating_an_inactive_user_starts_disabled() {
    let h = harness(|_| {}).await;
    let (status, created, _) = h
        .scim(
            "POST",
            "/scim/v2/Users",
            Some(json!({ "userName": "off@corp.example", "active": false })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(created["active"], false);
    let id = created["id"].as_str().unwrap();
    let (status, fetched, _) = h.scim("GET", &format!("/scim/v2/Users/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(fetched["active"], false);
}

#[tokio::test]
async fn unsupported_patches_are_rejected() {
    let h = harness(|_| {}).await;
    let (_, created, _) = h
        .scim(
            "POST",
            "/scim/v2/Users",
            Some(json!({ "userName": "ada@corp.example" })),
        )
        .await;
    let id = created["id"].as_str().unwrap();

    // unsupported op
    let (status, error, _) = h
        .scim(
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            Some(json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{ "op": "remove", "path": "active" }],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["scimType"], "invalidValue");

    // unsupported path
    let (status, error, _) = h
        .scim(
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            Some(json!({
                "schemas": ["urn:ietf:params:scim:api:messages:2.0:PatchOp"],
                "Operations": [{ "op": "replace", "path": "title", "value": "boss" }],
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["scimType"], "invalidPath");

    // missing PatchOp schema
    let (status, error, _) = h
        .scim(
            "PATCH",
            &format!("/scim/v2/Users/{id}"),
            Some(json!({ "Operations": [{ "op": "replace", "path": "active", "value": false }] })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["scimType"], "invalidValue");
}
