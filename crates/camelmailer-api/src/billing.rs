//! The billing seam: a provider-neutral [`BillingProvider`] trait and the
//! organization billing endpoints built on it.
//!
//! The open-source build ships [`NoBilling`] (the default, billing off) and
//! [`MockBilling`] for tests. A real provider lives outside this repository:
//! a separate crate implements the trait and builds its own binary through
//! `camelmailer::Extensions`, so self-hosted installations never carry a
//! payment integration.
//!
//! The dashboard renders what a provider hands back as an [`EmbedSession`]:
//! a web component (`element`) loaded from `script_url` and initialised with
//! a short-lived `token` and optional `attributes`, or, when the provider
//! has no component, a plain `url` to open. A provider decides which of the
//! two it offers per session.
//!
//! Billing is active only when `billing.enabled` is set **and** the
//! installed provider reports [`BillingProvider::enabled`]. Otherwise
//! `GET …/billing` reports `enabled: false` and every other billing
//! endpoint answers 403 `BillingDisabled`. Provider errors are logged and
//! never forwarded: callers only see the stable `BillingUnavailable` code.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::app::{
    find_organization, render_error, render_not_found, render_parameter_missing,
    render_store_error, render_success, ApiResponse, ApiState, Principal, RequestStart,
};

/// A failed interaction with the billing provider. The message is for the
/// server log only; API handlers map it to `BillingUnavailable`.
#[derive(Debug)]
pub struct BillingError(pub String);

impl std::fmt::Display for BillingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BillingError {}

/// The organization a billing call is about, as the provider sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BillingOrganization {
    pub id: camelmailer_core::Id,
    pub uuid: String,
    pub permalink: String,
    pub name: String,
    /// The provider customer id once [`BillingProvider::ensure_customer`]
    /// returned one (stored in `organizations.billing_customer_id`).
    pub customer_id: Option<String>,
    /// The email address of the acting user, when the request carries a
    /// user session (machine keys have none).
    pub user_email: Option<String>,
    /// The dashboard URL to come back to (`auth.frontend_url`), if known.
    pub return_url: Option<String>,
}

/// What the dashboard needs to show a provider-supplied UI.
///
/// With `element` and `script_url` set, the dashboard loads the script once
/// and renders `<{element} token="{token}" {attributes…}>`. With only `url`
/// set it shows a link (or redirects). Both may be set; the link is then the
/// fallback when the script cannot load.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbedSession {
    /// URL of the script that defines the custom element.
    pub script_url: Option<String>,
    /// Custom element name, e.g. `rs-checkout`. Must contain a hyphen.
    pub element: Option<String>,
    /// Short-lived token passed as the element's `token` attribute.
    pub token: Option<String>,
    /// Extra attributes for the element.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Plain link fallback (hosted checkout or portal page).
    pub url: Option<String>,
    /// When the token stops working, if the provider says.
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// A price as the provider states it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Price {
    /// Amount in the currency's minor unit (cents).
    pub amount_minor: i64,
    /// ISO 4217 code, e.g. `EUR`.
    pub currency: String,
    /// Billing interval, e.g. `month` or `year`.
    pub interval: Option<String>,
}

/// One plan a customer can choose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// Stable code passed back to [`BillingProvider::checkout_session`].
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    /// Outgoing messages per month included in the plan, if limited.
    pub monthly_send_limit: Option<i64>,
    pub price: Option<Price>,
    /// Short feature bullets for display.
    #[serde(default)]
    pub features: Vec<String>,
}

/// The billing backend of an installation.
#[async_trait::async_trait]
pub trait BillingProvider: Send + Sync {
    /// A short name for logs and the status endpoint, e.g. `none`.
    fn name(&self) -> &str;

    /// Whether this provider actually bills. [`NoBilling`] answers `false`.
    fn enabled(&self) -> bool {
        true
    }

    /// Create (or look up on the provider side) the customer for an
    /// organization and return its provider customer id. Called once per
    /// organization; the result is stored and passed back afterwards in
    /// [`BillingOrganization::customer_id`].
    async fn ensure_customer(&self, org: &BillingOrganization) -> Result<String, BillingError>;

    /// Start a checkout for `plan_code`.
    async fn checkout_session(
        &self,
        org: &BillingOrganization,
        plan_code: &str,
    ) -> Result<EmbedSession, BillingError>;

    /// Open the customer's subscription management (plan, payment method,
    /// invoices).
    async fn portal_session(&self, org: &BillingOrganization)
        -> Result<EmbedSession, BillingError>;

    /// A provider-rendered usage view, if the provider has one. The
    /// dashboard always shows Camelmailer's own usage figure as well.
    async fn usage_session(
        &self,
        _org: &BillingOrganization,
    ) -> Result<Option<EmbedSession>, BillingError> {
        Ok(None)
    }

    /// The plans on offer.
    async fn plans(&self) -> Result<Vec<Plan>, BillingError>;
}

// ------------------------------------------------------------ NoBilling

/// The default provider: billing is off. Every call fails, and
/// [`BillingProvider::enabled`] is `false`, so the endpoints never reach it.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBilling;

#[async_trait::async_trait]
impl BillingProvider for NoBilling {
    fn name(&self) -> &str {
        "none"
    }

    fn enabled(&self) -> bool {
        false
    }

    async fn ensure_customer(&self, _org: &BillingOrganization) -> Result<String, BillingError> {
        Err(BillingError("billing is not enabled".into()))
    }

    async fn checkout_session(
        &self,
        _org: &BillingOrganization,
        _plan_code: &str,
    ) -> Result<EmbedSession, BillingError> {
        Err(BillingError("billing is not enabled".into()))
    }

    async fn portal_session(
        &self,
        _org: &BillingOrganization,
    ) -> Result<EmbedSession, BillingError> {
        Err(BillingError("billing is not enabled".into()))
    }

    async fn plans(&self) -> Result<Vec<Plan>, BillingError> {
        Ok(Vec::new())
    }
}

// ---------------------------------------------------------------- mock

/// In-memory [`BillingProvider`] for tests: counts calls, records the last
/// organization, fails on demand. Sessions point at `https://billing.example`.
#[derive(Default)]
pub struct MockBilling {
    customers_created: AtomicUsize,
    sessions_created: AtomicUsize,
    fail: AtomicBool,
    last_organization: Mutex<Option<BillingOrganization>>,
    last_plan: Mutex<Option<String>>,
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

    pub fn sessions_created(&self) -> usize {
        self.sessions_created.load(Ordering::SeqCst)
    }

    pub fn last_organization(&self) -> Option<BillingOrganization> {
        self.last_organization.lock().unwrap().clone()
    }

    pub fn last_plan(&self) -> Option<String> {
        self.last_plan.lock().unwrap().clone()
    }

    fn check(&self, org: Option<&BillingOrganization>) -> Result<(), BillingError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(BillingError("mock: failure requested".into()));
        }
        if let Some(org) = org {
            *self.last_organization.lock().unwrap() = Some(org.clone());
        }
        Ok(())
    }

    fn session(&self, element: &str, org: &BillingOrganization) -> EmbedSession {
        self.sessions_created.fetch_add(1, Ordering::SeqCst);
        let customer = org.customer_id.clone().unwrap_or_default();
        EmbedSession {
            script_url: Some("https://billing.example/elements.js".into()),
            element: Some(element.into()),
            token: Some(format!("tok_{element}_{customer}")),
            attributes: BTreeMap::new(),
            url: Some(format!("https://billing.example/{element}/{customer}")),
            expires_at: None,
        }
    }
}

#[async_trait::async_trait]
impl BillingProvider for MockBilling {
    fn name(&self) -> &str {
        "mock"
    }

    async fn ensure_customer(&self, org: &BillingOrganization) -> Result<String, BillingError> {
        self.check(Some(org))?;
        let n = self.customers_created.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("cus_mock_{n}"))
    }

    async fn checkout_session(
        &self,
        org: &BillingOrganization,
        plan_code: &str,
    ) -> Result<EmbedSession, BillingError> {
        self.check(Some(org))?;
        *self.last_plan.lock().unwrap() = Some(plan_code.to_string());
        Ok(self.session("rs-checkout", org))
    }

    async fn portal_session(
        &self,
        org: &BillingOrganization,
    ) -> Result<EmbedSession, BillingError> {
        self.check(Some(org))?;
        Ok(self.session("rs-subscription", org))
    }

    async fn usage_session(
        &self,
        org: &BillingOrganization,
    ) -> Result<Option<EmbedSession>, BillingError> {
        self.check(Some(org))?;
        Ok(Some(self.session("rs-usage", org)))
    }

    async fn plans(&self) -> Result<Vec<Plan>, BillingError> {
        self.check(None)?;
        Ok(vec![Plan {
            code: "base".into(),
            name: "Base".into(),
            description: Some("Mock plan".into()),
            monthly_send_limit: Some(5000),
            price: Some(Price {
                amount_minor: 500,
                currency: "EUR".into(),
                interval: Some("month".into()),
            }),
            features: vec!["5,000 emails per month".into()],
        }])
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

fn render_billing_disabled(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::FORBIDDEN,
        "BillingDisabled",
        "Billing is not enabled on this installation",
    )
}

/// Whether billing is on: configured and backed by a real provider.
pub(crate) fn billing_active(state: &ApiState) -> bool {
    state.config.billing.enabled && state.billing.enabled()
}

/// `GET /api/v2/admin/organizations/{org}/billing` (admin/owner).
///
/// Always 200 for authorized callers: `{ enabled: false, … }` tells the
/// dashboard to show usage only.
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
    if !billing_active(&state) {
        return render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "enabled": false, "provider": null, "has_customer": false }),
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
        json!({
            "enabled": true,
            "provider": state.billing.name(),
            "has_customer": has_customer,
        }),
    )
}

/// Resolve the organization, require active billing, and make sure the
/// provider customer exists (created on first use, stored only after the
/// provider reported success).
async fn billing_organization(
    state: &ApiState,
    start: &RequestStart,
    principal: &Principal,
    org_permalink: &str,
) -> Result<BillingOrganization, ApiResponse> {
    let organization = match find_organization(state, org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return Err(render_not_found(Some(start))),
        Err(error) => return Err(render_store_error(Some(start), error)),
    };
    if !billing_active(state) {
        return Err(render_billing_disabled(Some(start)));
    }
    let existing = state
        .store
        .organization_billing_customer_id(organization.id)
        .await
        .map_err(|error| render_store_error(Some(start), error))?;
    let mut org = BillingOrganization {
        id: organization.id,
        uuid: organization.uuid.clone(),
        permalink: organization.permalink.clone(),
        name: organization.name.clone(),
        customer_id: existing,
        user_email: principal.user().map(|user| user.email_address.clone()),
        return_url: state.config.auth.frontend_url.clone(),
    };
    if org.customer_id.is_none() {
        let customer_id = match state.billing.ensure_customer(&org).await {
            Ok(customer_id) => customer_id,
            Err(error) => {
                tracing::error!(%error, organization = %org.permalink,
                    "creating the billing customer failed");
                return Err(render_billing_unavailable(Some(start)));
            }
        };
        state
            .store
            .set_organization_billing_customer_id(organization.id, &customer_id)
            .await
            .map_err(|error| render_store_error(Some(start), error))?;
        org.customer_id = Some(customer_id);
    }
    Ok(org)
}

fn render_session(
    start: &RequestStart,
    org: &BillingOrganization,
    what: &str,
    result: Result<EmbedSession, BillingError>,
) -> ApiResponse {
    match result {
        Ok(session) => render_success(
            Some(start),
            StatusCode::OK,
            json!({ "url": session.url, "session": session }),
        ),
        Err(error) => {
            tracing::error!(%error, organization = %org.permalink, what, "billing session failed");
            render_billing_unavailable(Some(start))
        }
    }
}

/// `GET /api/v2/admin/organizations/{org}/billing/plans` (admin/owner).
pub(crate) async fn billing_plans(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    match find_organization(&state, &org_permalink).await {
        Ok(Some(_)) => {}
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    }
    if !billing_active(&state) {
        return render_billing_disabled(Some(&start.0));
    }
    match state.billing.plans().await {
        Ok(plans) => render_success(Some(&start.0), StatusCode::OK, json!({ "plans": plans })),
        Err(error) => {
            tracing::error!(%error, "listing billing plans failed");
            render_billing_unavailable(Some(&start.0))
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CheckoutBody {
    plan_code: Option<String>,
}

/// `POST /api/v2/admin/organizations/{org}/billing/checkout` (admin/owner):
/// `{ plan_code }` → `{ url, session }`.
pub(crate) async fn billing_checkout(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path(org_permalink): Path<String>,
    body: Option<Json<CheckoutBody>>,
) -> ApiResponse {
    let Some(plan_code) = body
        .and_then(|Json(body)| body.plan_code)
        .filter(|code| !code.trim().is_empty())
    else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: plan_code",
        );
    };
    let org = match billing_organization(&state, &start.0, &principal.0, &org_permalink).await {
        Ok(org) => org,
        Err(response) => return response,
    };
    let result = state.billing.checkout_session(&org, &plan_code).await;
    render_session(&start.0, &org, "checkout", result)
}

/// `POST /api/v2/admin/organizations/{org}/billing/portal` (admin/owner or
/// admin API key) → `{ url, session }`.
pub(crate) async fn billing_portal(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    let org = match billing_organization(&state, &start.0, &principal.0, &org_permalink).await {
        Ok(org) => org,
        Err(response) => return response,
    };
    let result = state.billing.portal_session(&org).await;
    render_session(&start.0, &org, "portal", result)
}

/// `POST /api/v2/admin/organizations/{org}/billing/usage` (admin/owner) →
/// `{ session }`, `session` null when the provider has no usage view.
pub(crate) async fn billing_usage_session(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    let org = match billing_organization(&state, &start.0, &principal.0, &org_permalink).await {
        Ok(org) => org,
        Err(response) => return response,
    };
    match state.billing.usage_session(&org).await {
        Ok(session) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "session": session }),
        ),
        Err(error) => {
            tracing::error!(%error, organization = %org.permalink, "billing usage session failed");
            render_billing_unavailable(Some(&start.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org() -> BillingOrganization {
        BillingOrganization {
            id: 1,
            uuid: "u".into(),
            permalink: "acme".into(),
            name: "Acme".into(),
            customer_id: None,
            user_email: None,
            return_url: None,
        }
    }

    #[tokio::test]
    async fn no_billing_is_disabled_and_refuses_every_call() {
        let provider = NoBilling;
        assert!(!provider.enabled());
        assert_eq!(provider.name(), "none");
        assert!(provider.ensure_customer(&org()).await.is_err());
        assert!(provider.checkout_session(&org(), "base").await.is_err());
        assert!(provider.portal_session(&org()).await.is_err());
        assert_eq!(provider.usage_session(&org()).await.unwrap(), None);
        assert!(provider.plans().await.unwrap().is_empty());
    }

    #[test]
    fn an_embed_session_serializes_every_field() {
        let session = EmbedSession {
            script_url: Some("https://b.example/x.js".into()),
            element: Some("rs-checkout".into()),
            token: Some("t".into()),
            attributes: BTreeMap::from([("locale".into(), "de".into())]),
            url: None,
            expires_at: None,
        };
        let value = serde_json::to_value(&session).unwrap();
        assert_eq!(value["element"], json!("rs-checkout"));
        assert_eq!(value["attributes"]["locale"], json!("de"));
        assert_eq!(value["url"], json!(null));
    }
}
