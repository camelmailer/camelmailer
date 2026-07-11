//! Stripe billing for the hosted cloud offering.
//!
//! Entirely opt-in via the `billing` config group: self-hosted
//! installations keep the default (`enabled: false`), which makes
//! `GET …/billing` report `enabled: false` (the dashboard then shows no
//! billing UI at all) and `POST …/billing/portal` answer 403
//! `BillingDisabled`.
//!
//! The Stripe integration is deliberately SDK-free: two form-encoded
//! `POST`s against the public REST API (`/v1/customers`,
//! `/v1/billing_portal/sessions`) authenticated with the secret key as a
//! Bearer token. Stripe error details are logged, never forwarded to the
//! client — callers only ever see the stable `BillingUnavailable` code.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::{
    find_organization, render_error, render_not_found, render_store_error, render_success,
    ApiResponse, ApiState, Principal, RequestStart,
};

/// A failed interaction with the billing provider. The message is for the
/// server log only — API handlers must map this to the generic
/// `BillingUnavailable` error and never forward the detail to the client.
#[derive(Debug)]
pub struct BillingError(pub String);

impl std::fmt::Display for BillingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BillingError {}

/// The billing backend: Stripe in production ([`StripeBilling`]), an
/// in-memory fake in tests ([`MockBilling`]).
#[async_trait::async_trait]
pub trait BillingProvider: Send + Sync {
    /// Create (or look up on the provider side) the customer for an
    /// organization and return its provider customer id. `email` is the
    /// address of the acting user when the request carries a user session
    /// (machine keys have none).
    async fn ensure_customer(
        &self,
        org_permalink: &str,
        org_name: &str,
        email: Option<&str>,
    ) -> Result<String, BillingError>;

    /// Create a billing-portal session for the customer and return the
    /// URL to redirect the browser to.
    async fn portal_session(
        &self,
        customer_id: &str,
        return_url: Option<&str>,
    ) -> Result<String, BillingError>;
}

// -------------------------------------------------------------- Stripe

/// Stripe over plain HTTPS (reqwest), no SDK.
pub struct StripeBilling {
    secret_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl StripeBilling {
    pub fn new(secret_key: String) -> Self {
        Self::with_base_url(secret_key, "https://api.stripe.com".into())
    }

    /// Point the client at a different host — used by tests to talk to a
    /// local mock server instead of Stripe.
    pub fn with_base_url(secret_key: String, base_url: String) -> Self {
        Self {
            secret_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn customers_url(&self) -> String {
        format!("{}/v1/customers", self.base_url)
    }

    fn portal_sessions_url(&self) -> String {
        format!("{}/v1/billing_portal/sessions", self.base_url)
    }

    /// One form-encoded POST against the Stripe API. Provider errors are
    /// logged here with full detail; the returned [`BillingError`] is only
    /// ever logged again, never rendered to a client.
    async fn post_form(
        &self,
        url: &str,
        form: &[(&'static str, String)],
        what: &str,
    ) -> Result<serde_json::Value, BillingError> {
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.secret_key)
            .form(form)
            .send()
            .await
            .map_err(|error| {
                tracing::error!(%error, what, "stripe request failed");
                BillingError(format!("{what}: request failed: {error}"))
            })?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            tracing::error!(%status, body, what, "stripe returned an error");
            return Err(BillingError(format!("{what}: stripe returned {status}")));
        }
        serde_json::from_str(&body).map_err(|error| {
            tracing::error!(%error, what, "stripe returned invalid JSON");
            BillingError(format!("{what}: invalid JSON: {error}"))
        })
    }
}

/// The form body of `POST /v1/customers` — kept as a pure function so the
/// encoding is unit-testable without HTTP.
fn customer_form(
    org_permalink: &str,
    org_name: &str,
    email: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("name", org_name.to_string()),
        ("metadata[org_permalink]", org_permalink.to_string()),
    ];
    if let Some(email) = email.filter(|e| !e.is_empty()) {
        form.push(("email", email.to_string()));
    }
    form
}

/// The form body of `POST /v1/billing_portal/sessions`.
fn portal_form(customer_id: &str, return_url: Option<&str>) -> Vec<(&'static str, String)> {
    let mut form = vec![("customer", customer_id.to_string())];
    if let Some(url) = return_url.filter(|u| !u.is_empty()) {
        form.push(("return_url", url.to_string()));
    }
    form
}

#[async_trait::async_trait]
impl BillingProvider for StripeBilling {
    async fn ensure_customer(
        &self,
        org_permalink: &str,
        org_name: &str,
        email: Option<&str>,
    ) -> Result<String, BillingError> {
        let form = customer_form(org_permalink, org_name, email);
        let body = self
            .post_form(&self.customers_url(), &form, "create customer")
            .await?;
        body["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BillingError("create customer: response carries no id".into()))
    }

    async fn portal_session(
        &self,
        customer_id: &str,
        return_url: Option<&str>,
    ) -> Result<String, BillingError> {
        let form = portal_form(customer_id, return_url);
        let body = self
            .post_form(&self.portal_sessions_url(), &form, "create portal session")
            .await?;
        body["url"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| BillingError("create portal session: response carries no url".into()))
    }
}

// ---------------------------------------------------------------- mock

/// The recorded arguments of the last `ensure_customer` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MockCustomerRequest {
    pub org_permalink: String,
    pub org_name: String,
    pub email: Option<String>,
}

/// In-memory [`BillingProvider`] for tests: counts calls, records
/// arguments, fails on demand.
#[derive(Default)]
pub struct MockBilling {
    customers_created: AtomicUsize,
    portal_sessions_created: AtomicUsize,
    fail: AtomicBool,
    last_customer_request: Mutex<Option<MockCustomerRequest>>,
    last_portal_customer: Mutex<Option<String>>,
}

impl MockBilling {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Make every subsequent provider call fail (`true`) or succeed.
    pub fn set_fail(&self, fail: bool) {
        self.fail.store(fail, Ordering::SeqCst);
    }

    pub fn customers_created(&self) -> usize {
        self.customers_created.load(Ordering::SeqCst)
    }

    pub fn portal_sessions_created(&self) -> usize {
        self.portal_sessions_created.load(Ordering::SeqCst)
    }

    pub fn last_customer_request(&self) -> Option<MockCustomerRequest> {
        self.last_customer_request.lock().unwrap().clone()
    }

    pub fn last_portal_customer(&self) -> Option<String> {
        self.last_portal_customer.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl BillingProvider for MockBilling {
    async fn ensure_customer(
        &self,
        org_permalink: &str,
        org_name: &str,
        email: Option<&str>,
    ) -> Result<String, BillingError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(BillingError("mock: ensure_customer failed".into()));
        }
        let n = self.customers_created.fetch_add(1, Ordering::SeqCst) + 1;
        *self.last_customer_request.lock().unwrap() = Some(MockCustomerRequest {
            org_permalink: org_permalink.to_string(),
            org_name: org_name.to_string(),
            email: email.map(str::to_string),
        });
        Ok(format!("cus_mock_{n}"))
    }

    async fn portal_session(
        &self,
        customer_id: &str,
        _return_url: Option<&str>,
    ) -> Result<String, BillingError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(BillingError("mock: portal_session failed".into()));
        }
        self.portal_sessions_created.fetch_add(1, Ordering::SeqCst);
        *self.last_portal_customer.lock().unwrap() = Some(customer_id.to_string());
        Ok(format!(
            "https://billing.stripe.example/session/{customer_id}"
        ))
    }
}

// ------------------------------------------------------------ handlers

fn render_billing_unavailable(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::BAD_GATEWAY,
        "BillingUnavailable",
        "The billing provider is currently unavailable; please try again later",
    )
}

/// `GET /api/v2/admin/organizations/{org}/billing` (admin/owner).
///
/// Always 200 for authorized callers: `{ enabled: false, … }` simply tells
/// the frontend to hide billing (self-hosted installations).
pub(crate) async fn billing_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    let organization = match find_organization(&state, &org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    if !state.config.billing.enabled || state.billing.is_none() {
        return render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "enabled": false, "has_customer": false }),
        );
    }
    let has_customer = match state
        .store
        .organization_billing_customer_id(organization.id)
        .await
    {
        Ok(customer_id) => customer_id.is_some(),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "enabled": true, "has_customer": has_customer }),
    )
}

/// `POST /api/v2/admin/organizations/{org}/billing/portal` (admin/owner or
/// admin API key).
///
/// Creates the Stripe customer on first use (idempotent: an existing
/// customer is reused), then opens a billing-portal session and returns
/// its `{ url }`. The customer id is persisted only after Stripe reported
/// success, so a failed call leaves nothing half-stored.
pub(crate) async fn billing_portal(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    let organization = match find_organization(&state, &org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let provider = match (state.config.billing.enabled, state.billing.as_ref()) {
        (true, Some(provider)) => provider,
        _ => {
            return render_error(
                Some(&start.0),
                StatusCode::FORBIDDEN,
                "BillingDisabled",
                "Billing is not enabled on this installation",
            )
        }
    };

    let existing = match state
        .store
        .organization_billing_customer_id(organization.id)
        .await
    {
        Ok(existing) => existing,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let customer_id = match existing {
        Some(customer_id) => customer_id,
        None => {
            let email = principal.user().map(|user| user.email_address.as_str());
            let customer_id = match provider
                .ensure_customer(&organization.permalink, &organization.name, email)
                .await
            {
                Ok(customer_id) => customer_id,
                Err(error) => {
                    tracing::error!(%error, organization = %organization.permalink,
                        "creating the billing customer failed");
                    return render_billing_unavailable(Some(&start.0));
                }
            };
            // Persist only after the provider reported success.
            if let Err(error) = state
                .store
                .set_organization_billing_customer_id(organization.id, &customer_id)
                .await
            {
                return render_store_error(Some(&start.0), error);
            }
            customer_id
        }
    };

    let return_url = state.config.billing.portal_return_url.as_deref().or(state
        .config
        .auth
        .frontend_url
        .as_deref());
    match provider.portal_session(&customer_id, return_url).await {
        Ok(url) => render_success(Some(&start.0), StatusCode::OK, json!({ "url": url })),
        Err(error) => {
            tracing::error!(%error, organization = %organization.permalink,
                "creating the billing portal session failed");
            render_billing_unavailable(Some(&start.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a form exactly like `RequestBuilder::form` does (build the
    /// request without sending it and inspect the body bytes).
    fn encode(form: &[(&'static str, String)]) -> String {
        let request = reqwest::Client::new()
            .post("http://localhost/ignored")
            .form(form)
            .build()
            .unwrap();
        String::from_utf8(request.body().unwrap().as_bytes().unwrap().to_vec()).unwrap()
    }

    #[test]
    fn customer_form_encodes_name_metadata_and_email() {
        let form = customer_form("acme-org", "Acme & Söhne", Some("owner@acme.example"));
        assert_eq!(
            encode(&form),
            "name=Acme+%26+S%C3%B6hne&metadata%5Borg_permalink%5D=acme-org&email=owner%40acme.example"
        );
    }

    #[test]
    fn customer_form_omits_a_missing_or_empty_email() {
        assert_eq!(
            encode(&customer_form("acme", "Acme", None)),
            "name=Acme&metadata%5Borg_permalink%5D=acme"
        );
        assert_eq!(
            encode(&customer_form("acme", "Acme", Some(""))),
            "name=Acme&metadata%5Borg_permalink%5D=acme"
        );
    }

    #[test]
    fn portal_form_encodes_customer_and_optional_return_url() {
        assert_eq!(
            encode(&portal_form(
                "cus_123",
                Some("https://app.example.com/billing?x=1")
            )),
            "customer=cus_123&return_url=https%3A%2F%2Fapp.example.com%2Fbilling%3Fx%3D1"
        );
        assert_eq!(encode(&portal_form("cus_123", None)), "customer=cus_123");
    }

    #[test]
    fn stripe_urls_are_built_from_the_base_url() {
        let stripe = StripeBilling::new("sk_test_1".into());
        assert_eq!(
            stripe.customers_url(),
            "https://api.stripe.com/v1/customers"
        );
        assert_eq!(
            stripe.portal_sessions_url(),
            "https://api.stripe.com/v1/billing_portal/sessions"
        );

        // Trailing slashes are normalized, mock hosts work.
        let stripe = StripeBilling::with_base_url("sk".into(), "http://127.0.0.1:9999/".into());
        assert_eq!(stripe.customers_url(), "http://127.0.0.1:9999/v1/customers");
        assert_eq!(
            stripe.portal_sessions_url(),
            "http://127.0.0.1:9999/v1/billing_portal/sessions"
        );
    }
}
