//! Template push between servers of one organization:
//! `POST …/servers/{server}/templates/{permalink}/copy_to`.
//! Covers the copy itself, permalink conflicts + overwrite, the 404 wall
//! around foreign organizations, and RBAC (member may copy, viewer not).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, build_router, ApiState};
use camelmailer_core::{
    auth, AdminStore, AuthStore, MemoryStore, NewOrganization, NewServer, NewTemplate, NewUser,
    Role, ServerMode, ServerStore, StoreError,
};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use std::sync::OnceLock;
use tower::ServiceExt;

const ADMIN_KEY: &str = "admin-key-000000000000";
const PASSWORD: &str = "correct-horse-battery";

fn password_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| auth::hash_password(PASSWORD).unwrap())
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
    alpha_id: u64,
    beta_id: u64,
}

/// One org ("acme") with two servers ("alpha", "beta"), a template on
/// alpha, and a second org ("other") with its own server ("gamma").
async fn harness() -> Harness {
    let store = Arc::new(MemoryStore::new());
    let acme = store
        .create_organization(NewOrganization {
            name: "Acme".into(),
            permalink: "acme".into(),
        })
        .await
        .unwrap();
    let alpha = store
        .create_server(NewServer {
            organization_id: acme.id,
            name: "Alpha".into(),
            permalink: "alpha".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let beta = store
        .create_server(NewServer {
            organization_id: acme.id,
            name: "Beta".into(),
            permalink: "beta".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    let other = store
        .create_organization(NewOrganization {
            name: "Other".into(),
            permalink: "other".into(),
        })
        .await
        .unwrap();
    store
        .create_server(NewServer {
            organization_id: other.id,
            name: "Gamma".into(),
            permalink: "gamma".into(),
            mode: ServerMode::Live,
        })
        .await
        .unwrap();
    ServerStore::create_template(
        store.as_ref(),
        NewTemplate {
            server_id: alpha.id,
            name: "Welcome".into(),
            permalink: "welcome".into(),
            subject: Some("Hello {{ name }}".into()),
            html_body: Some("<p>Hi {{ name }}</p>".into()),
            text_body: Some("Hi {{ name }}".into()),
            layout_id: None,
        },
    )
    .await
    .unwrap();

    let state = ApiState::full(
        store.clone(),
        Some(store.clone()),
        Some(store.clone()),
        Some(ADMIN_KEY.into()),
        camelmailer_config::Config::default(),
    );
    let app = build_router(state.clone()).merge(build_auth_router(state));
    Harness {
        app,
        store,
        alpha_id: alpha.id,
        beta_id: beta.id,
    }
}

impl Harness {
    async fn member(&self, email: &str, role: Role) {
        let user = self
            .store
            .create_user(NewUser {
                email_address: email.into(),
                first_name: "Test".into(),
                last_name: "User".into(),
                admin: false,
            })
            .await
            .unwrap();
        self.store
            .set_password_digest(user.id, password_digest())
            .await
            .unwrap();
        let org = self
            .store
            .organization_by_permalink("acme")
            .await
            .unwrap()
            .unwrap();
        self.store
            .upsert_membership(org.id, user.id, role)
            .await
            .unwrap();
    }

    async fn login(&self, email: &str) -> String {
        let (status, body) = self
            .request(
                "POST",
                "/api/v2/auth/login",
                &[],
                Some(json!({ "email_address": email, "password": PASSWORD })),
            )
            .await;
        assert_eq!(status, StatusCode::CREATED, "login failed: {body}");
        body["data"]["session_token"].as_str().unwrap().to_string()
    }

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

    async fn copy(
        &self,
        headers: &[(&str, String)],
        source_server: &str,
        permalink: &str,
        body: Value,
    ) -> (StatusCode, Value) {
        self.request(
            "POST",
            &format!(
                "/api/v2/admin/organizations/acme/servers/{source_server}/templates/{permalink}/copy_to"
            ),
            headers,
            Some(body),
        )
        .await
    }

    async fn template_on(
        &self,
        server_id: u64,
        permalink: &str,
    ) -> Result<Option<camelmailer_core::Template>, StoreError> {
        ServerStore::template_by_permalink(self.store.as_ref(), server_id, permalink).await
    }
}

fn admin() -> Vec<(&'static str, String)> {
    vec![("X-Admin-API-Key", ADMIN_KEY.to_string())]
}

#[tokio::test]
async fn copies_a_template_to_a_sibling_server() {
    let h = harness().await;
    let (status, body) = h
        .copy(
            &admin(),
            "alpha",
            "welcome",
            json!({ "target_server": "beta" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(body["data"]["template"]["permalink"], "welcome");
    assert_eq!(body["data"]["overwritten"], false);

    let copied = h.template_on(h.beta_id, "welcome").await.unwrap().unwrap();
    assert_eq!(copied.name, "Welcome");
    assert_eq!(copied.subject.as_deref(), Some("Hello {{ name }}"));
    assert_eq!(copied.html_body.as_deref(), Some("<p>Hi {{ name }}</p>"));
    assert_eq!(copied.text_body.as_deref(), Some("Hi {{ name }}"));
    // the source is untouched
    assert!(h
        .template_on(h.alpha_id, "welcome")
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn an_existing_permalink_conflicts_unless_overwrite_is_set() {
    let h = harness().await;
    ServerStore::create_template(
        h.store.as_ref(),
        NewTemplate {
            server_id: h.beta_id,
            name: "Old welcome".into(),
            permalink: "welcome".into(),
            subject: Some("Old subject".into()),
            html_body: None,
            text_body: Some("Old".into()),
            layout_id: None,
        },
    )
    .await
    .unwrap();

    let (status, body) = h
        .copy(
            &admin(),
            "alpha",
            "welcome",
            json!({ "target_server": "beta" }),
        )
        .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"]["code"], "ValidationError");

    let (status, body) = h
        .copy(
            &admin(),
            "alpha",
            "welcome",
            json!({ "target_server": "beta", "overwrite": true }),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["overwritten"], true);
    let replaced = h.template_on(h.beta_id, "welcome").await.unwrap().unwrap();
    assert_eq!(replaced.name, "Welcome");
    assert_eq!(replaced.subject.as_deref(), Some("Hello {{ name }}"));
    assert_eq!(replaced.text_body.as_deref(), Some("Hi {{ name }}"));
}

#[tokio::test]
async fn unknown_targets_and_foreign_orgs_are_a_404_wall() {
    let h = harness().await;

    // a server of ANOTHER organization is indistinguishable from an
    // unknown permalink
    let (status, body) = h
        .copy(
            &admin(),
            "alpha",
            "welcome",
            json!({ "target_server": "gamma" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(body["error"]["code"], "NotFound");

    let (status, _) = h
        .copy(
            &admin(),
            "alpha",
            "welcome",
            json!({ "target_server": "nope" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // unknown source template
    let (status, _) = h
        .copy(
            &admin(),
            "alpha",
            "missing",
            json!({ "target_server": "beta" }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // missing target_server parameter
    let (status, body) = h.copy(&admin(), "alpha", "welcome", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "ParameterMissing");

    // and nothing was copied anywhere
    assert!(h.template_on(h.beta_id, "welcome").await.unwrap().is_none());
}

#[tokio::test]
async fn members_may_copy_viewers_may_not_and_foreign_users_see_404() {
    let h = harness().await;
    h.member("member@example.com", Role::Member).await;
    h.member("viewer@example.com", Role::Viewer).await;

    let viewer = h.login("viewer@example.com").await;
    let (status, body) = h
        .copy(
            &[("authorization", format!("Bearer {viewer}"))],
            "alpha",
            "welcome",
            json!({ "target_server": "beta" }),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], "Forbidden");

    let member = h.login("member@example.com").await;
    let (status, body) = h
        .copy(
            &[("authorization", format!("Bearer {member}"))],
            "alpha",
            "welcome",
            json!({ "target_server": "beta" }),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");

    // a user without a membership in the org gets a 404 (not a 403)
    let stranger = h
        .store
        .create_user(NewUser {
            email_address: "stranger@example.com".into(),
            first_name: "No".into(),
            last_name: "Access".into(),
            admin: false,
        })
        .await
        .unwrap();
    h.store
        .set_password_digest(stranger.id, password_digest())
        .await
        .unwrap();
    let stranger = h.login("stranger@example.com").await;
    let (status, _) = h
        .copy(
            &[("authorization", format!("Bearer {stranger}"))],
            "alpha",
            "welcome",
            json!({ "target_server": "beta", "overwrite": true }),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
