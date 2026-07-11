//! Stripe billing (hosted cloud): the org billing status + portal
//! endpoints against MockBilling, RBAC per the repository conventions,
//! and StripeBilling against a local mock HTTP server standing in for
//! the Stripe API.

use axum::body::Body;
use axum::extract::RawForm;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use camelmailer_api::{
    build_auth_router, build_router, ApiState, BillingProvider, MockBilling, StripeBilling,
};
use camelmailer_core::{AdminStore, AuthStore, MemoryStore, NewOrganization, NewUser, Role};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, OnceLock};
use tower::ServiceExt;

const PASSWORD: &str = "correct-horse-battery";
const ADMIN_KEY: &str = "machine-key-1";

fn password_digest() -> &'static str {
    static DIGEST: OnceLock<String> = OnceLock::new();
    DIGEST.get_or_init(|| camelmailer_core::auth::hash_password(PASSWORD).unwrap())
}

fn billing_config(enabled: bool) -> camelmailer_config::Config {
    let mut config = camelmailer_config::Config::default();
    config.billing.enabled = enabled;
    if enabled {
        config.billing.stripe_secret_key = Some("sk_test_1".into());
    }
    config.auth.frontend_url = Some("https://mail-admin.example.com".into());
    config.validate().unwrap();
    config
}

struct Harness {
    app: Router,
    store: Arc<MemoryStore>,
    billing: Arc<MockBilling>,
}

fn harness_with(config: camelmailer_config::Config, with_provider: bool) -> Harness {
    let store = Arc::new(MemoryStore::new());
    let billing = MockBilling::new();
    let provider: Option<Arc<dyn BillingProvider>> = if with_provider {
        Some(billing.clone())
    } else {
        None
    };
    let state = ApiState::full_with_billing(
        store.clone(),
        None,
        Some(store.clone()),
        Some(ADMIN_KEY.into()),
        config,
        provider,
    );
    let app = build_router(state.clone()).merge(build_auth_router(state));
    Harness {
        app,
        store,
        billing,
    }
}

/// The hosted-cloud shape: billing enabled with a MockBilling provider.
fn enabled_harness() -> Harness {
    harness_with(billing_config(true), true)
}

/// The self-hosted shape: billing disabled, no provider wired at all.
fn disabled_harness() -> Harness {
    harness_with(billing_config(false), false)
}

impl Harness {
    async fn org(&self, name: &str) -> camelmailer_core::Organization {
        self.store
            .create_organization(NewOrganization {
                name: name.into(),
                permalink: name.to_lowercase(),
            })
            .await
            .unwrap()
    }

    async fn member(
        &self,
        org: &camelmailer_core::Organization,
        email: &str,
        role: Role,
    ) -> camelmailer_core::User {
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
        self.store
            .upsert_membership(org.id, user.id, role)
            .await
            .unwrap();
        user
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
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
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
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn as_user(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let bearer = format!("Bearer {token}");
        self.request(method, path, &[("authorization", &bearer)], body)
            .await
    }
}

// ----------------------------------------------------- disabled (self-hosted)

#[tokio::test]
async fn disabled_billing_reports_enabled_false_on_get() {
    let h = disabled_harness();
    let org = h.org("Acme").await;
    h.member(&org, "owner@acme.example", Role::Owner).await;
    let token = h.login("owner@acme.example").await;

    let (status, body) = h
        .as_user(
            &token,
            "GET",
            "/api/v2/admin/organizations/acme/billing",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["enabled"], json!(false));
    assert_eq!(body["data"]["has_customer"], json!(false));
}

#[tokio::test]
async fn disabled_billing_rejects_the_portal_with_billing_disabled() {
    let h = disabled_harness();
    let org = h.org("Acme").await;
    h.member(&org, "owner@acme.example", Role::Owner).await;
    let token = h.login("owner@acme.example").await;

    let (status, body) = h
        .as_user(
            &token,
            "POST",
            "/api/v2/admin/organizations/acme/billing/portal",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"]["code"], json!("BillingDisabled"));
    // The provider was never touched and nothing was stored.
    assert_eq!(h.billing.customers_created(), 0);
    assert_eq!(
        h.store
            .organization_billing_customer_id(org.id)
            .await
            .unwrap(),
        None
    );
}

// ------------------------------------------------------- enabled (cloud)

#[tokio::test]
async fn first_portal_call_creates_the_customer_and_returns_the_url() {
    let h = enabled_harness();
    let org = h.org("Acme").await;
    h.member(&org, "admin@acme.example", Role::Admin).await;
    let token = h.login("admin@acme.example").await;

    // Before: enabled, but no customer yet.
    let (status, body) = h
        .as_user(
            &token,
            "GET",
            "/api/v2/admin/organizations/acme/billing",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["enabled"], json!(true));
    assert_eq!(body["data"]["has_customer"], json!(false));

    let (status, body) = h
        .as_user(
            &token,
            "POST",
            "/api/v2/admin/organizations/acme/billing/portal",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["data"]["url"],
        json!("https://billing.stripe.example/session/cus_mock_1")
    );

    // The customer was created once, with the acting user's email and the
    // org permalink as metadata, and persisted.
    assert_eq!(h.billing.customers_created(), 1);
    assert_eq!(h.billing.portal_sessions_created(), 1);
    let request = h.billing.last_customer_request().unwrap();
    assert_eq!(request.org_permalink, "acme");
    assert_eq!(request.org_name, "Acme");
    assert_eq!(request.email.as_deref(), Some("admin@acme.example"));
    assert_eq!(
        h.store
            .organization_billing_customer_id(org.id)
            .await
            .unwrap()
            .as_deref(),
        Some("cus_mock_1")
    );

    // And GET now reports the customer.
    let (status, body) = h
        .as_user(
            &token,
            "GET",
            "/api/v2/admin/organizations/acme/billing",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["has_customer"], json!(true));
}

#[tokio::test]
async fn second_portal_call_reuses_the_existing_customer() {
    let h = enabled_harness();
    let org = h.org("Acme").await;
    h.member(&org, "owner@acme.example", Role::Owner).await;
    let token = h.login("owner@acme.example").await;

    for _ in 0..2 {
        let (status, body) = h
            .as_user(
                &token,
                "POST",
                "/api/v2/admin/organizations/acme/billing/portal",
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["data"]["url"],
            json!("https://billing.stripe.example/session/cus_mock_1")
        );
    }
    // Idempotent: one customer, two portal sessions.
    assert_eq!(h.billing.customers_created(), 1);
    assert_eq!(h.billing.portal_sessions_created(), 2);
    assert_eq!(
        h.billing.last_portal_customer().as_deref(),
        Some("cus_mock_1")
    );
    assert_eq!(
        h.store
            .organization_billing_customer_id(org.id)
            .await
            .unwrap()
            .as_deref(),
        Some("cus_mock_1")
    );
}

#[tokio::test]
async fn the_admin_api_key_may_open_the_portal_without_an_email() {
    let h = enabled_harness();
    let org = h.org("Acme").await;

    let (status, body) = h
        .request(
            "POST",
            "/api/v2/admin/organizations/acme/billing/portal",
            &[("X-Admin-API-Key", ADMIN_KEY)],
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["data"]["url"]
        .as_str()
        .unwrap()
        .starts_with("https://"));
    let request = h.billing.last_customer_request().unwrap();
    assert_eq!(request.email, None);
    assert_eq!(
        h.store
            .organization_billing_customer_id(org.id)
            .await
            .unwrap()
            .as_deref(),
        Some("cus_mock_1")
    );
}

// ------------------------------------------------------------------ RBAC

#[tokio::test]
async fn members_and_viewers_are_forbidden_and_non_members_get_404() {
    let h = enabled_harness();
    let org = h.org("Acme").await;
    h.org("Other").await;
    h.member(&org, "member@acme.example", Role::Member).await;
    h.member(&org, "viewer@acme.example", Role::Viewer).await;

    for email in ["member@acme.example", "viewer@acme.example"] {
        let token = h.login(email).await;
        let (status, body) = h
            .as_user(
                &token,
                "GET",
                "/api/v2/admin/organizations/acme/billing",
                None,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{email}: {body}");
        assert_eq!(body["error"]["code"], json!("Forbidden"));
        let (status, body) = h
            .as_user(
                &token,
                "POST",
                "/api/v2/admin/organizations/acme/billing/portal",
                None,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{email}: {body}");
        assert_eq!(body["error"]["code"], json!("Forbidden"));
    }

    // Foreign orgs answer 404 (existence is not leaked), like unknown ones.
    let token = h.login("member@acme.example").await;
    for path in [
        "/api/v2/admin/organizations/other/billing",
        "/api/v2/admin/organizations/no-such-org/billing",
    ] {
        let (status, body) = h.as_user(&token, "GET", path, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}: {body}");
        assert_eq!(body["error"]["code"], json!("NotFound"));
    }
    assert_eq!(h.billing.customers_created(), 0);
}

// ---------------------------------------------------------- failure path

#[tokio::test]
async fn provider_failure_maps_to_billing_unavailable_and_stores_nothing() {
    let h = enabled_harness();
    let org = h.org("Acme").await;
    h.member(&org, "owner@acme.example", Role::Owner).await;
    let token = h.login("owner@acme.example").await;

    h.billing.set_fail(true);
    let (status, body) = h
        .as_user(
            &token,
            "POST",
            "/api/v2/admin/organizations/acme/billing/portal",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");
    assert_eq!(body["error"]["code"], json!("BillingUnavailable"));
    // The provider's message is not forwarded to the client.
    assert!(!body["error"]["message"].as_str().unwrap().contains("mock:"));
    // Nothing half-stored.
    assert_eq!(
        h.store
            .organization_billing_customer_id(org.id)
            .await
            .unwrap(),
        None
    );
    let (_, body) = h
        .as_user(
            &token,
            "GET",
            "/api/v2/admin/organizations/acme/billing",
            None,
        )
        .await;
    assert_eq!(body["data"]["has_customer"], json!(false));

    // A later retry (provider back up) succeeds and creates the customer.
    h.billing.set_fail(false);
    let (status, body) = h
        .as_user(
            &token,
            "POST",
            "/api/v2/admin/organizations/acme/billing/portal",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.billing.customers_created(), 1);
}

// -------------------------------------- StripeBilling against a mock API

/// A minimal stand-in for the Stripe REST API: records the Authorization
/// header and the raw form bodies, answers like Stripe would.
struct MockStripe {
    base_url: String,
    requests: Arc<Mutex<Vec<(String, String, String)>>>,
}

async fn start_mock_stripe(fail_customers: bool) -> MockStripe {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests: Arc<Mutex<Vec<(String, String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let customers_log = requests.clone();
    let sessions_log = requests.clone();
    let app = Router::new()
        .route(
            "/v1/customers",
            post(move |headers: HeaderMap, RawForm(body): RawForm| {
                let log = customers_log.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    log.lock().unwrap().push((
                        "/v1/customers".into(),
                        auth,
                        String::from_utf8(body.to_vec()).unwrap(),
                    ));
                    if fail_customers {
                        (
                            StatusCode::PAYMENT_REQUIRED,
                            Json(json!({ "error": { "message": "Your card was declined." } })),
                        )
                    } else {
                        (StatusCode::OK, Json(json!({ "id": "cus_stripe_1" })))
                    }
                }
            }),
        )
        .route(
            "/v1/billing_portal/sessions",
            post(move |headers: HeaderMap, RawForm(body): RawForm| {
                let log = sessions_log.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or("")
                        .to_string();
                    log.lock().unwrap().push((
                        "/v1/billing_portal/sessions".into(),
                        auth,
                        String::from_utf8(body.to_vec()).unwrap(),
                    ));
                    (
                        StatusCode::OK,
                        Json(json!({ "url": "https://billing.stripe.com/p/session/test_123" })),
                    )
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    MockStripe { base_url, requests }
}

#[tokio::test]
async fn stripe_billing_posts_form_encoded_requests_with_bearer_auth() {
    let stripe = start_mock_stripe(false).await;
    let provider = StripeBilling::with_base_url("sk_test_abc".into(), stripe.base_url.clone());

    let customer_id = provider
        .ensure_customer("acme", "Acme & Söhne", Some("owner@acme.example"))
        .await
        .unwrap();
    assert_eq!(customer_id, "cus_stripe_1");

    let url = provider
        .portal_session(&customer_id, Some("https://app.example.com/billing"))
        .await
        .unwrap();
    assert_eq!(url, "https://billing.stripe.com/p/session/test_123");

    let requests = stripe.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let (path, auth, body) = &requests[0];
    assert_eq!(path, "/v1/customers");
    assert_eq!(auth, "Bearer sk_test_abc");
    assert_eq!(
        body,
        "name=Acme+%26+S%C3%B6hne&metadata%5Borg_permalink%5D=acme&email=owner%40acme.example"
    );
    let (path, auth, body) = &requests[1];
    assert_eq!(path, "/v1/billing_portal/sessions");
    assert_eq!(auth, "Bearer sk_test_abc");
    assert_eq!(
        body,
        "customer=cus_stripe_1&return_url=https%3A%2F%2Fapp.example.com%2Fbilling"
    );
}

#[tokio::test]
async fn stripe_errors_are_mapped_and_not_leaked_verbatim() {
    let stripe = start_mock_stripe(true).await;
    let provider = StripeBilling::with_base_url("sk_test_abc".into(), stripe.base_url.clone());

    let error = provider
        .ensure_customer("acme", "Acme", None)
        .await
        .expect_err("a 4xx from stripe must fail");
    // The error carries the status for the log, but not Stripe's message —
    // that is only ever logged, never bubbled to clients.
    assert!(error.to_string().contains("402"), "{error}");
    assert!(!error.to_string().contains("card was declined"), "{error}");
}
