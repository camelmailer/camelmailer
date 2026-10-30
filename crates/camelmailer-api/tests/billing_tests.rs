//! The provider-neutral billing seam: the org billing status, plans,
//! checkout, portal and usage-session endpoints against MockBilling, the
//! NoBilling default, and RBAC per the repository conventions.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, build_router, ApiState, BillingProvider, MockBilling};
use camelmailer_core::{AdminStore, AuthStore, MemoryStore, NewOrganization, NewUser, Role};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
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
        json!("https://billing.example/rs-subscription/cus_mock_1")
    );
    // The embed session for the dashboard's web component.
    let session = &body["data"]["session"];
    assert_eq!(session["element"], json!("rs-subscription"));
    assert_eq!(session["token"], json!("tok_rs-subscription_cus_mock_1"));
    assert_eq!(
        session["script_url"],
        json!("https://billing.example/elements.js")
    );

    // The customer was created once, with the acting user's email, and
    // persisted; the provider saw the stored customer id afterwards.
    assert_eq!(h.billing.customers_created(), 1);
    assert_eq!(h.billing.sessions_created(), 1);
    let request = h.billing.last_organization().unwrap();
    assert_eq!(request.permalink, "acme");
    assert_eq!(request.name, "Acme");
    assert_eq!(request.user_email.as_deref(), Some("admin@acme.example"));
    assert_eq!(request.customer_id.as_deref(), Some("cus_mock_1"));
    assert_eq!(
        request.return_url.as_deref(),
        Some("https://mail-admin.example.com")
    );
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
            json!("https://billing.example/rs-subscription/cus_mock_1")
        );
    }
    // Idempotent: one customer, two portal sessions.
    assert_eq!(h.billing.customers_created(), 1);
    assert_eq!(h.billing.sessions_created(), 2);
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
    let request = h.billing.last_organization().unwrap();
    assert_eq!(request.user_email, None);
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

// ------------------------------------------------- plans, checkout, usage

#[tokio::test]
async fn plans_checkout_and_usage_sessions_go_through_the_provider() {
    let h = enabled_harness();
    let org = h.org("Acme").await;
    h.member(&org, "owner@acme.example", Role::Owner).await;
    let token = h.login("owner@acme.example").await;

    let (status, body) = h
        .as_user(
            &token,
            "GET",
            "/api/v2/admin/organizations/acme/billing/plans",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["plans"][0]["code"], json!("base"));
    assert_eq!(body["data"]["plans"][0]["monthly_send_limit"], json!(5000));
    assert_eq!(
        body["data"]["plans"][0]["price"],
        json!({ "amount_minor": 500, "currency": "EUR", "interval": "month" })
    );

    // A checkout needs a plan code.
    let (status, body) = h
        .as_user(
            &token,
            "POST",
            "/api/v2/admin/organizations/acme/billing/checkout",
            Some(json!({})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], json!("ParameterMissing"));

    let (status, body) = h
        .as_user(
            &token,
            "POST",
            "/api/v2/admin/organizations/acme/billing/checkout",
            Some(json!({ "plan_code": "base" })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["session"]["element"], json!("rs-checkout"));
    assert_eq!(h.billing.last_plan().as_deref(), Some("base"));
    // The checkout created the customer on first use.
    assert_eq!(h.billing.customers_created(), 1);

    let (status, body) = h
        .as_user(
            &token,
            "POST",
            "/api/v2/admin/organizations/acme/billing/usage",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["data"]["session"]["element"], json!("rs-usage"));
    assert_eq!(h.billing.customers_created(), 1, "the customer is reused");
}

#[tokio::test]
async fn billing_enabled_without_a_provider_stays_off() {
    // `billing.enabled: true` on the open-source build (NoBilling) must
    // behave like billing disabled, not fail at request time.
    let h = harness_with(billing_config(true), false);
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
    for path in ["billing/portal", "billing/checkout", "billing/usage"] {
        let (status, body) = h
            .as_user(
                &token,
                "POST",
                &format!("/api/v2/admin/organizations/acme/{path}"),
                Some(json!({ "plan_code": "base" })),
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}: {body}");
        assert_eq!(body["error"]["code"], json!("BillingDisabled"));
    }
    let (status, _) = h
        .as_user(
            &token,
            "GET",
            "/api/v2/admin/organizations/acme/billing/plans",
            None,
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn the_features_endpoint_reports_billing() {
    for (harness, expected) in [(enabled_harness(), true), (disabled_harness(), false)] {
        let (status, body) = harness
            .request("GET", "/api/v2/auth/features", &[], None)
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["data"]["billing"], json!(expected));
        assert_eq!(body["data"]["captcha"]["provider"], json!("none"));
        assert_eq!(body["data"]["captcha"]["endpoint"], json!(null));
    }
}
