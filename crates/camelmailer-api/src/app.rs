//! The Admin API v2 router — the Rust port of
//! `app/controllers/admin_api/base_controller.rb` and the resource
//! controllers.
//!
//! Conventions preserved from the Ruby implementation:
//! - authentication via the `X-Admin-API-Key` header, checked first against
//!   database-backed admin API keys and then against the global
//!   `camelmailer.admin_api_key` config value (constant-time compare)
//! - every response is `{ status, time, data | error }`
//! - list endpoints paginate with `page` / `per_page` (capped at 100)
//! - `NotFound` → 404, `ValidationError` → 422, `ParameterMissing` → 400

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use camelmailer_core::{
    AdminStore, NewOrganization as CoreNewOrganization, NewServer as CoreNewServer, Organization,
    Server, ServerMode, StoreError,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Instant;
use subtle::ConstantTimeEq;

use crate::resources;

#[derive(Clone)]
pub struct ApiState {
    /// Storage — in-memory for tests, PostgreSQL in production.
    pub store: Arc<dyn AdminStore>,
    /// Tenant-scoped storage for the per-server API (`/api/v2/server`).
    pub server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
    /// Account/session storage for user logins and RBAC (`/api/v2/auth`).
    pub auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
    /// `camelmailer.admin_api_key` — the global fallback key.
    pub global_admin_api_key: Option<String>,
    /// The full configuration (auth policy, OIDC, hostnames).
    pub config: camelmailer_config::Config,
    /// Billing backend (hosted cloud only). `None` on self-hosted
    /// installations: billing endpoints then report disabled.
    pub billing: Option<Arc<dyn crate::billing::BillingProvider>>,
    /// Live TXT lookups for `POST …/domains/{name}/verify` — hickory in
    /// production, a static mock in tests.
    pub dns_resolver: Arc<dyn camelmailer_core::DnsResolver>,
    /// base64(SubjectPublicKeyInfo) of the installation signing key — the
    /// DKIM `p=` value for domains without their own key.
    pub installation_dkim_public_key: Option<String>,
    /// GitHub's OAuth surface for `auth.sso_providers` — the real client
    /// in production, a mock in tests.
    pub sso_github: Arc<dyn crate::sso::GithubOauth>,
    /// The HTTP leg of synchronous webhook test deliveries — reqwest with
    /// a 10 s timeout in production; tests may inject a short timeout.
    pub webhook_sender: Arc<dyn crate::webhook_send::WebhookSender>,
    /// PEM of the installation signing key (`camelmailer.signing_key_path`)
    /// — signs webhook test payloads exactly like the worker. `None` when
    /// the key file does not exist (signing is then skipped).
    pub installation_signing_key_pem: Option<String>,
    /// Per-organization SSO configuration (tenant OIDC/SAML/social). `None`
    /// when accounts storage is not enabled; the dashboard endpoints then
    /// report the feature unavailable.
    pub org_sso_store: Option<Arc<dyn camelmailer_core::OrgSsoStore>>,
}

impl ApiState {
    pub fn new(store: Arc<dyn AdminStore>, global_admin_api_key: Option<String>) -> Arc<Self> {
        Self::new_with_resolver(
            store,
            global_admin_api_key,
            Arc::new(crate::dns::HickoryDnsResolver),
        )
    }

    /// [`ApiState::new`] with an injected DNS resolver (tests).
    pub fn new_with_resolver(
        store: Arc<dyn AdminStore>,
        global_admin_api_key: Option<String>,
        dns_resolver: Arc<dyn camelmailer_core::DnsResolver>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            server_store: None,
            auth_store: None,
            global_admin_api_key,
            config: camelmailer_config::Config::default(),
            billing: None,
            dns_resolver,
            installation_dkim_public_key: None,
            sso_github: Arc::new(crate::sso::HttpGithub::default()),
            webhook_sender: Arc::new(crate::webhook_send::ReqwestWebhookSender::new()),
            installation_signing_key_pem: None,
            org_sso_store: None,
        })
    }

    /// Construct with tenant-scoped storage for the per-server API.
    pub fn with_server_store(
        store: Arc<dyn AdminStore>,
        server_store: Arc<dyn camelmailer_core::ServerStore>,
        global_admin_api_key: Option<String>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            server_store: Some(server_store),
            auth_store: None,
            global_admin_api_key,
            config: camelmailer_config::Config::default(),
            billing: None,
            dns_resolver: Arc::new(crate::dns::HickoryDnsResolver),
            installation_dkim_public_key: None,
            sso_github: Arc::new(crate::sso::HttpGithub::default()),
            webhook_sender: Arc::new(crate::webhook_send::ReqwestWebhookSender::new()),
            installation_signing_key_pem: None,
            org_sso_store: None,
        })
    }

    /// Construct with every storage facet plus configuration — the shape
    /// used by the production binary (and account-aware tests).
    pub fn full(
        store: Arc<dyn AdminStore>,
        server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
        auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
        global_admin_api_key: Option<String>,
        config: camelmailer_config::Config,
    ) -> Arc<Self> {
        Self::full_with_resolver(
            store,
            server_store,
            auth_store,
            global_admin_api_key,
            config,
            Arc::new(crate::dns::HickoryDnsResolver),
        )
    }

    /// [`ApiState::full`] plus a billing backend (hosted cloud).
    pub fn full_with_billing(
        store: Arc<dyn AdminStore>,
        server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
        auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
        global_admin_api_key: Option<String>,
        config: camelmailer_config::Config,
        billing: Option<Arc<dyn crate::billing::BillingProvider>>,
    ) -> Arc<Self> {
        Self::full_with(
            store,
            server_store,
            auth_store,
            global_admin_api_key,
            config,
            billing,
            Arc::new(crate::dns::HickoryDnsResolver),
        )
    }

    /// [`ApiState::full`] with an injected DNS resolver (tests).
    pub fn full_with_resolver(
        store: Arc<dyn AdminStore>,
        server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
        auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
        global_admin_api_key: Option<String>,
        config: camelmailer_config::Config,
        dns_resolver: Arc<dyn camelmailer_core::DnsResolver>,
    ) -> Arc<Self> {
        Self::full_with(
            store,
            server_store,
            auth_store,
            global_admin_api_key,
            config,
            None,
            dns_resolver,
        )
    }

    /// [`ApiState::full`] with an explicit GitHub OAuth client — router
    /// tests inject one pointing at a local mock GitHub.
    pub fn full_with_github(
        store: Arc<dyn AdminStore>,
        server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
        auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
        global_admin_api_key: Option<String>,
        config: camelmailer_config::Config,
        sso_github: Arc<dyn crate::sso::GithubOauth>,
    ) -> Arc<Self> {
        Self::full_with_all(
            store,
            server_store,
            auth_store,
            global_admin_api_key,
            config,
            None,
            Arc::new(crate::dns::HickoryDnsResolver),
            sso_github,
        )
    }

    /// The common constructor behind [`ApiState::full_with_billing`],
    /// [`ApiState::full_with_resolver`] and [`ApiState::full_with_github`].
    pub fn full_with(
        store: Arc<dyn AdminStore>,
        server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
        auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
        global_admin_api_key: Option<String>,
        config: camelmailer_config::Config,
        billing: Option<Arc<dyn crate::billing::BillingProvider>>,
        dns_resolver: Arc<dyn camelmailer_core::DnsResolver>,
    ) -> Arc<Self> {
        Self::full_with_all(
            store,
            server_store,
            auth_store,
            global_admin_api_key,
            config,
            billing,
            dns_resolver,
            Arc::new(crate::sso::HttpGithub::default()),
        )
    }

    /// The fully explicit constructor.
    #[allow(clippy::too_many_arguments)]
    pub fn full_with_all(
        store: Arc<dyn AdminStore>,
        server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
        auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
        global_admin_api_key: Option<String>,
        config: camelmailer_config::Config,
        billing: Option<Arc<dyn crate::billing::BillingProvider>>,
        dns_resolver: Arc<dyn camelmailer_core::DnsResolver>,
        sso_github: Arc<dyn crate::sso::GithubOauth>,
    ) -> Arc<Self> {
        Self::full_with_webhook_sender(
            store,
            server_store,
            auth_store,
            global_admin_api_key,
            config,
            billing,
            dns_resolver,
            sso_github,
            Arc::new(crate::webhook_send::ReqwestWebhookSender::new()),
        )
    }

    /// [`ApiState::full_with_all`] with an explicit webhook sender —
    /// router tests inject one with a short timeout.
    #[allow(clippy::too_many_arguments)]
    pub fn full_with_webhook_sender(
        store: Arc<dyn AdminStore>,
        server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
        auth_store: Option<Arc<dyn camelmailer_core::AuthStore>>,
        global_admin_api_key: Option<String>,
        config: camelmailer_config::Config,
        billing: Option<Arc<dyn crate::billing::BillingProvider>>,
        dns_resolver: Arc<dyn camelmailer_core::DnsResolver>,
        sso_github: Arc<dyn crate::sso::GithubOauth>,
        webhook_sender: Arc<dyn crate::webhook_send::WebhookSender>,
    ) -> Arc<Self> {
        // The installation signing key doubles as the DKIM fallback for
        // domains without their own key (public half) and as the webhook
        // payload signing key (private half), when it exists.
        let installation_signing_key_pem =
            std::fs::read_to_string(&config.camelmailer.signing_key_path).ok();
        let installation_dkim_public_key = installation_signing_key_pem
            .as_deref()
            .and_then(crate::resources::dkim_public_key_b64);
        Arc::new(Self {
            store,
            server_store,
            auth_store,
            global_admin_api_key,
            config,
            billing,
            dns_resolver,
            installation_dkim_public_key,
            sso_github,
            webhook_sender,
            installation_signing_key_pem,
            org_sso_store: None,
        })
    }

    /// Attach per-organization SSO storage (production and SSO-aware
    /// tests). The state is otherwise immutable behind an `Arc`, so this
    /// returns a fresh one with the store installed.
    pub fn with_org_sso_store(
        self: Arc<Self>,
        org_sso_store: Arc<dyn camelmailer_core::OrgSsoStore>,
    ) -> Arc<Self> {
        let mut state = (*self).clone();
        state.org_sso_store = Some(org_sso_store);
        Arc::new(state)
    }

    async fn key_is_valid(&self, key: &str) -> bool {
        // 1. database-backed admin API keys (records their use)
        if self.store.admin_api_key_valid(key).await.unwrap_or(false) {
            return true;
        }
        // 2. the configured global key, compared in constant time
        match &self.global_admin_api_key {
            Some(configured) if !configured.is_empty() => {
                configured.as_bytes().ct_eq(key.as_bytes()).into()
            }
            _ => false,
        }
    }
}

/// Per-request timer, injected before auth so even 401s carry `time`.
#[derive(Clone, Copy)]
pub(crate) struct RequestStart(pub(crate) Instant);

pub(crate) fn elapsed(request_start: Option<&RequestStart>) -> f64 {
    let seconds = request_start
        .map(|start| start.0.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    (seconds * 1000.0).round() / 1000.0
}

pub(crate) struct ApiResponse {
    pub(crate) status: StatusCode,
    pub(crate) body: Value,
}

impl IntoResponse for ApiResponse {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

pub(crate) fn render_success(
    start: Option<&RequestStart>,
    status: StatusCode,
    data: Value,
) -> ApiResponse {
    ApiResponse {
        status,
        body: json!({
            "status": "success",
            "time": elapsed(start),
            "data": data,
        }),
    }
}

pub(crate) fn render_error(
    start: Option<&RequestStart>,
    status: StatusCode,
    code: &str,
    message: &str,
) -> ApiResponse {
    ApiResponse {
        status,
        body: json!({
            "status": "error",
            "time": elapsed(start),
            "error": { "code": code, "message": message },
        }),
    }
}

pub(crate) fn render_deleted(start: Option<&RequestStart>) -> ApiResponse {
    render_success(start, StatusCode::OK, json!({ "deleted": true }))
}

pub(crate) fn render_not_found(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::NOT_FOUND,
        "NotFound",
        "Resource not found",
    )
}

pub(crate) fn render_validation_error(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(
        start,
        StatusCode::UNPROCESSABLE_ENTITY,
        "ValidationError",
        message,
    )
}

pub(crate) fn render_parameter_missing(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(start, StatusCode::BAD_REQUEST, "ParameterMissing", message)
}

pub(crate) fn render_store_error(start: Option<&RequestStart>, error: StoreError) -> ApiResponse {
    match error {
        StoreError::Conflict(message) => render_validation_error(start, &message),
        StoreError::Other(message) => {
            tracing::error!(%message, "storage error");
            render_error(
                start,
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerError",
                "An internal error occurred",
            )
        }
    }
}

pub(crate) async fn timing_middleware(mut request: Request, next: Next) -> Response {
    request
        .extensions_mut()
        .insert(RequestStart(Instant::now()));
    next.run(request).await
}

/// The authenticated caller of an admin API request: a machine key (full
/// access) or a signed-in user (subject to RBAC).
#[derive(Clone)]
pub(crate) enum Principal {
    AdminKey,
    User(camelmailer_core::User),
}

impl Principal {
    /// Full, unscoped access: the machine key or a global admin account.
    pub(crate) fn is_root(&self) -> bool {
        match self {
            Principal::AdminKey => true,
            Principal::User(user) => user.admin,
        }
    }

    pub(crate) fn user(&self) -> Option<&camelmailer_core::User> {
        match self {
            Principal::AdminKey => None,
            Principal::User(user) => Some(user),
        }
    }
}

/// The caller's role within the organization named in the request path
/// (`None` for root principals, which bypass role checks).
#[derive(Clone, Copy)]
pub(crate) struct ActingRole(pub(crate) Option<camelmailer_core::Role>);

impl ActingRole {
    pub(crate) fn is_owner(&self) -> bool {
        self.0.is_none() || self.0 == Some(camelmailer_core::Role::Owner)
    }
}

/// The minimum role required for a request under
/// `/api/v2/admin/organizations/{permalink}/…`, by resource and method.
fn required_role(rest: &[&str], method: &axum::http::Method) -> camelmailer_core::Role {
    use axum::http::Method;
    use camelmailer_core::Role;
    let read = *method == Method::GET || *method == Method::HEAD;
    match rest.first() {
        // the organization itself: any write (PATCH `require_two_factor`,
        // DELETE) is reserved for owners
        None => {
            if read {
                Role::Viewer
            } else {
                Role::Owner
            }
        }
        // people management
        Some(&"members") | Some(&"invitations") => {
            if read {
                Role::Viewer
            } else {
                Role::Admin
            }
        }
        // billing (hosted cloud): reads and the portal are both admin+
        Some(&"billing") => Role::Admin,
        // tenant SSO configuration holds provider secrets; only admins and
        // owners may see or change it.
        Some(&"sso") => Role::Admin,
        Some(&"servers") => match rest.get(2) {
            // server lifecycle (create/update/delete/suspend/ip_pool)
            None | Some(&"suspend") | Some(&"unsuspend") | Some(&"ip_pool") => {
                if read {
                    Role::Viewer
                } else {
                    Role::Admin
                }
            }
            // resources within a server (domains, credentials, routes, …)
            Some(_) => {
                if read {
                    Role::Viewer
                } else {
                    Role::Member
                }
            }
        },
        // anything unrecognized: readable by members, writable by admins
        _ => {
            if read {
                Role::Viewer
            } else {
                Role::Admin
            }
        }
    }
}

async fn auth_middleware(
    State(state): State<Arc<ApiState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let start = request.extensions().get::<RequestStart>().copied();

    // 1. machine key
    let key = request
        .headers()
        .get("X-Admin-API-Key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !key.is_empty() {
        let key = key.to_string();
        if !state.key_is_valid(&key).await {
            return render_error(
                start.as_ref(),
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "Invalid API key",
            )
            .into_response();
        }
        request.extensions_mut().insert(Principal::AdminKey);
        request.extensions_mut().insert(ActingRole(None));
        return next.run(request).await;
    }

    // 2. user session (Bearer), when accounts are enabled
    let bearer = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty());
    let (Some(token), Some(auth_store)) = (bearer, state.auth_store.clone()) else {
        return render_error(
            start.as_ref(),
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "Missing X-Admin-API-Key header or Authorization: Bearer session token",
        )
        .into_response();
    };
    let token_hash = camelmailer_core::auth::hash_token(&token);
    let now = chrono::Utc::now();
    let session = match auth_store.session_with_user(&token_hash).await {
        Ok(session) => session,
        Err(error) => return render_store_error(start.as_ref(), error).into_response(),
    };
    let Some((session, user)) = session.filter(|(session, _)| session.expires_at > now) else {
        return render_error(
            start.as_ref(),
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "The session token is invalid or has expired",
        )
        .into_response();
    };
    let _ = auth_store
        .touch_session(
            session.id,
            now,
            now + chrono::Duration::days(state.config.auth.session_timeout_days as i64),
        )
        .await;

    // 3. RBAC for user principals
    // Inside the nested router the URI is already stripped of the
    // `/api/v2/admin` prefix; handle both shapes.
    let path = request.uri().path().to_string();
    let segments: Vec<&str> = path
        .strip_prefix("/api/v2/admin")
        .unwrap_or(&path)
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    let method = request.method().clone();
    let mut acting_role = ActingRole(None);

    // Resolve the organization for org-scoped paths once — the RBAC role
    // check and the org-wide 2FA enforcement below both need it.
    let organization = match (segments.first(), segments.get(1)) {
        (Some(&"organizations"), Some(permalink)) => {
            match state.store.organization_by_permalink(permalink).await {
                Ok(organization) => organization,
                Err(error) => return render_store_error(start.as_ref(), error).into_response(),
            }
        }
        _ => None,
    };

    if !user.admin {
        match segments.first() {
            Some(&"organizations") => match segments.get(1) {
                // index (handler filters to memberships) / create
                None => {
                    if method == axum::http::Method::POST
                        && !state.config.auth.allow_organization_creation
                    {
                        return render_error(
                            start.as_ref(),
                            StatusCode::FORBIDDEN,
                            "Forbidden",
                            "Organization creation is restricted to administrators",
                        )
                        .into_response();
                    }
                }
                Some(_) => {
                    // Unknown org and org-without-membership answer
                    // identically so existence is not leaked.
                    let membership = match &organization {
                        Some(organization) => {
                            match auth_store.membership(organization.id, user.id).await {
                                Ok(membership) => membership,
                                Err(error) => {
                                    return render_store_error(start.as_ref(), error)
                                        .into_response()
                                }
                            }
                        }
                        None => None,
                    };
                    let Some(membership) = membership else {
                        return render_not_found(start.as_ref()).into_response();
                    };
                    let required = required_role(&segments[2..], &method);
                    if membership.role < required {
                        return render_error(
                            start.as_ref(),
                            StatusCode::FORBIDDEN,
                            "Forbidden",
                            &format!(
                                "This action requires the {} role in this organization",
                                required.as_str()
                            ),
                        )
                        .into_response();
                    }
                    acting_role = ActingRole(Some(membership.role));
                }
            },
            // global resources (users, ip_pools, admin_api_keys, auth_events)
            _ => {
                return render_error(
                    start.as_ref(),
                    StatusCode::FORBIDDEN,
                    "Forbidden",
                    "This action requires a global administrator account",
                )
                .into_response();
            }
        }
    }

    // Org-wide 2FA enforcement (Postmark-style). Applies to every session
    // principal touching an organization that requires two-factor —
    // including global admins (no backdoor). Admin API keys returned
    // earlier and are unaffected; non-members already got their 404 above.
    if let Some(organization) = &organization {
        if organization.require_two_factor {
            match auth_store.user_has_two_factor(user.id).await {
                Ok(true) => {}
                Ok(false) => {
                    return render_error(
                        start.as_ref(),
                        StatusCode::FORBIDDEN,
                        "TwoFactorEnforced",
                        "This organization requires two-factor authentication. \
                         Enable two-factor authentication on your account to continue.",
                    )
                    .into_response();
                }
                Err(error) => return render_store_error(start.as_ref(), error).into_response(),
            }
        }
    }

    request.extensions_mut().insert(Principal::User(user));
    request.extensions_mut().insert(acting_role);
    next.run(request).await
}

// ------------------------------------------------------------- pagination

#[derive(Debug, Deserialize, Default)]
pub(crate) struct PaginationParams {
    pub(crate) page: Option<u64>,
    pub(crate) per_page: Option<u64>,
}

pub(crate) struct Paginated<T> {
    pub(crate) items: Vec<T>,
    pub(crate) pagination: Value,
}

pub(crate) fn paginate<T: Clone>(collection: &[T], params: &PaginationParams) -> Paginated<T> {
    let page = params.page.unwrap_or(1).max(1);
    let per_page = params.per_page.unwrap_or(25).clamp(1, 100);
    let total = collection.len() as u64;
    let total_pages = total.div_ceil(per_page);
    let offset = ((page - 1) * per_page) as usize;
    let items = collection
        .iter()
        .skip(offset)
        .take(per_page as usize)
        .cloned()
        .collect();
    Paginated {
        items,
        pagination: json!({
            "page": page,
            "per_page": per_page,
            "total": total,
            "total_pages": total_pages,
        }),
    }
}

// ---------------------------------------------------------- serialization

fn organization_json(organization: &Organization) -> Value {
    json!({
        "id": organization.id,
        "uuid": organization.uuid,
        "name": organization.name,
        "permalink": organization.permalink,
        "require_two_factor": organization.require_two_factor,
    })
}

pub(crate) fn server_json(server: &Server) -> Value {
    json!({
        "id": server.id,
        "uuid": server.uuid,
        "name": server.name,
        "permalink": server.permalink,
        "mode": match server.mode { ServerMode::Live => "Live", ServerMode::Development => "Development" },
        "suspended": server.suspended,
        "suspension_reason": server.suspension_reason,
        "privacy_mode": server.privacy_mode,
        "track_opens": server.track_opens,
        "track_clicks": server.track_clicks,
        "spam_threshold": server.spam_threshold,
        "outbound_spam_threshold": server.outbound_spam_threshold,
        "bounce_hook_url": server.bounce_hook_url,
        "delivery_hook_url": server.delivery_hook_url,
        "inbound_domain": server.inbound_domain,
        "broadcast_physical_address": server.broadcast_physical_address,
        "color": server.color,
        "ip_pool_id": server.ip_pool_id,
        "default_stream_id": server.default_stream_id,
    })
}

// ---------------------------------------------------------- organizations

async fn organizations_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    // Root principals see every organization; users see the ones they
    // are members of.
    let mut organizations = if principal.is_root() {
        match state.store.list_organizations().await {
            Ok(organizations) => organizations,
            Err(error) => return render_store_error(Some(&start.0), error),
        }
    } else {
        let user = principal.user().expect("non-root principal is a user");
        let Some(auth_store) = state.auth_store.as_ref() else {
            return render_success(
                Some(&start.0),
                StatusCode::OK,
                json!({ "organizations": [], "pagination": paginate::<Value>(&[], &params).pagination }),
            );
        };
        match auth_store.memberships_for_user(user.id).await {
            Ok(memberships) => memberships
                .into_iter()
                .map(|(_, organization)| organization)
                .collect(),
            Err(error) => return render_store_error(Some(&start.0), error),
        }
    };
    organizations.sort_by(|a, b| a.name.cmp(&b.name));
    let result = paginate(&organizations, &params);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "organizations": result.items.iter().map(organization_json).collect::<Vec<_>>(),
            "pagination": result.pagination,
        }),
    )
}

#[derive(Debug, Deserialize)]
struct CreateOrganization {
    name: Option<String>,
    permalink: Option<String>,
}

async fn organizations_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Json(body): Json<CreateOrganization>,
) -> ApiResponse {
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: name",
        );
    };
    let permalink = body
        .permalink
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| permalink_from(&name));

    match state
        .store
        .create_organization(CoreNewOrganization { name, permalink })
        .await
    {
        Ok(organization) => {
            // A user creating an organization becomes its owner.
            if let (Some(user), Some(auth_store)) = (principal.user(), state.auth_store.as_ref()) {
                if let Err(error) = auth_store
                    .upsert_membership(organization.id, user.id, camelmailer_core::Role::Owner)
                    .await
                {
                    return render_store_error(Some(&start.0), error);
                }
            }
            render_success(
                Some(&start.0),
                StatusCode::CREATED,
                json!({ "organization": organization_json(&organization) }),
            )
        }
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

pub(crate) async fn find_organization(
    state: &ApiState,
    permalink: &str,
) -> Result<Option<Organization>, StoreError> {
    state.store.organization_by_permalink(permalink).await
}

async fn organizations_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "organization": organization_json(&organization) }),
        ),
        Ok(None) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize, Default)]
struct UpdateOrganization {
    require_two_factor: Option<bool>,
}

/// `PATCH /organizations/{permalink}` — update organization settings.
/// Owner-only (enforced centrally via [`required_role`]); currently the
/// one mutable setting is `require_two_factor` (org-wide 2FA enforcement).
async fn organizations_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
    Json(body): Json<UpdateOrganization>,
) -> ApiResponse {
    let mut organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(require_two_factor) = body.require_two_factor else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: require_two_factor",
        );
    };
    organization.require_two_factor = require_two_factor;
    match state.store.update_organization(organization).await {
        Ok(organization) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "organization": organization_json(&organization) }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

async fn organizations_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => match state.store.delete_organization(organization.id).await {
            Ok(_) => render_deleted(Some(&start.0)),
            Err(error) => render_store_error(Some(&start.0), error),
        },
        Ok(None) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

// ---------------------------------------------------------------- servers

async fn servers_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let organization = match find_organization(&state, &org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let servers: Vec<Server> = match state.store.servers_for_organization(organization.id).await {
        Ok(servers) => servers,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let result = paginate(&servers, &params);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "servers": result.items.iter().map(server_json).collect::<Vec<_>>(),
            "pagination": result.pagination,
        }),
    )
}

/// `GET /organizations/{permalink}/servers/stats` — per-server 30-day
/// message counters for the dashboard's servers table. RBAC is identical
/// to the servers index (a read any org member may perform); the central
/// [`auth_middleware`] enforces it, non-members get 404. Each server's
/// aggregate comes from [`camelmailer_core::ServerStore::message_stats`]
/// over a `from = now - 30 days` window; servers with no messages report
/// zeros and are still included.
async fn servers_stats(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    let organization = match find_organization(&state, &org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(server_store) = state.server_store.as_ref() else {
        return render_error(
            Some(&start.0),
            StatusCode::SERVICE_UNAVAILABLE,
            "StorageUnavailable",
            "Per-server statistics storage is not configured",
        );
    };
    let servers: Vec<Server> = match state.store.servers_for_organization(organization.id).await {
        Ok(servers) => servers,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let filter = camelmailer_core::StatsFilter {
        from: Some(chrono::Utc::now() - chrono::Duration::days(30)),
        to: None,
        tag: None,
    };
    let mut stats = Vec::with_capacity(servers.len());
    for server in &servers {
        let counters = match server_store.message_stats(server.id, &filter).await {
            Ok(counters) => counters,
            Err(error) => return render_store_error(Some(&start.0), error),
        };
        stats.push(json!({
            "server": server.permalink,
            "total": counters.total,
            "outgoing": counters.outgoing,
            "incoming": counters.incoming,
            "bounced": counters.bounced,
        }));
    }
    render_success(Some(&start.0), StatusCode::OK, json!({ "stats": stats }))
}

/// `GET /organizations/{permalink}/servers/{server_permalink}/stats` — the
/// full 30-day (windowed) message + engagement counters for a single server,
/// for the admin dashboard's detailed delivery/bounce/engagement view. RBAC
/// is a read any org member may perform (Viewer), enforced centrally by
/// [`auth_middleware`]; non-members get 404. Optional `from`/`to` RFC3339
/// query params scope the window; without `from` it defaults to the last 30
/// days. The JSON shape is identical to `GET /api/v2/server/stats` — it reuses
/// the same [`crate::server_api::stats_json`] serialization.
async fn server_stats_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, server_permalink)): Path<(String, String)>,
    Query(params): Query<crate::server_api::StatsParams>,
) -> ApiResponse {
    let server = match find_server(&state, &org_permalink, &server_permalink).await {
        Ok(Some(server)) => server,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(server_store) = state.server_store.as_ref() else {
        return render_error(
            Some(&start.0),
            StatusCode::SERVICE_UNAVAILABLE,
            "StorageUnavailable",
            "Per-server statistics storage is not configured",
        );
    };
    let filter = camelmailer_core::StatsFilter {
        from: params
            .from
            .or_else(|| Some(chrono::Utc::now() - chrono::Duration::days(30))),
        to: params.to,
        tag: params.tag.filter(|t| !t.is_empty()),
    };
    match server_store.message_stats(server.id, &filter).await {
        Ok(stats) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "stats": crate::server_api::stats_json(&stats) }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize)]
struct CreateServer {
    name: Option<String>,
    permalink: Option<String>,
    mode: Option<String>,
}

async fn servers_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
    Json(body): Json<CreateServer>,
) -> ApiResponse {
    let organization = match find_organization(&state, &org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: name",
        );
    };
    let mode = match body.mode.as_deref() {
        None | Some("Live") => ServerMode::Live,
        Some("Development") => ServerMode::Development,
        Some(other) => {
            return render_validation_error(
                Some(&start.0),
                &format!("Mode {other:?} is not a valid mode"),
            )
        }
    };
    let permalink = body
        .permalink
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| permalink_from(&name));

    match state
        .store
        .create_server(CoreNewServer {
            organization_id: organization.id,
            name,
            permalink,
            mode,
        })
        .await
    {
        Ok(server) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({ "server": server_json(&server) }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

pub(crate) async fn find_server(
    state: &ApiState,
    org_permalink: &str,
    permalink: &str,
) -> Result<Option<Server>, StoreError> {
    let Some(organization) = find_organization(state, org_permalink).await? else {
        return Ok(None);
    };
    state
        .store
        .server_by_permalink(organization.id, permalink)
        .await
}

async fn servers_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink).await {
        Ok(Some(server)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "server": server_json(&server) }),
        ),
        Ok(None) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

async fn servers_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink).await {
        Ok(Some(server)) => match state.store.delete_server(server.id).await {
            Ok(_) => render_deleted(Some(&start.0)),
            Err(error) => render_store_error(Some(&start.0), error),
        },
        Ok(None) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize, Default)]
struct SuspendBody {
    reason: Option<String>,
}

async fn servers_suspend(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
    body: Option<Json<SuspendBody>>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink).await {
        Ok(Some(mut server)) => {
            server.suspended = true;
            server.suspension_reason = body
                .and_then(|Json(b)| b.reason)
                .or(Some("Suspended via Admin API".into()));
            match state.store.update_server(server).await {
                Ok(server) => render_success(
                    Some(&start.0),
                    StatusCode::OK,
                    json!({ "server": server_json(&server) }),
                ),
                Err(error) => render_store_error(Some(&start.0), error),
            }
        }
        Ok(None) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

async fn servers_unsuspend(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink).await {
        Ok(Some(mut server)) => {
            server.suspended = false;
            server.suspension_reason = None;
            match state.store.update_server(server).await {
                Ok(server) => render_success(
                    Some(&start.0),
                    StatusCode::OK,
                    json!({ "server": server_json(&server) }),
                ),
                Err(error) => render_store_error(Some(&start.0), error),
            }
        }
        Ok(None) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize, Default)]
struct UpdateServer {
    name: Option<String>,
    mode: Option<String>,
    track_opens: Option<bool>,
    track_clicks: Option<bool>,
    spam_threshold: Option<f64>,
    outbound_spam_threshold: Option<f64>,
    bounce_hook_url: Option<String>,
    delivery_hook_url: Option<String>,
    inbound_domain: Option<String>,
    broadcast_physical_address: Option<String>,
    color: Option<String>,
}

async fn servers_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
    Json(body): Json<UpdateServer>,
) -> ApiResponse {
    let mut server = match find_server(&state, &org_permalink, &permalink).await {
        Ok(Some(server)) => server,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    if let Some(name) = body.name {
        server.name = name;
    }
    if let Some(mode) = body.mode.as_deref() {
        server.mode = match mode {
            "Live" => ServerMode::Live,
            "Development" => ServerMode::Development,
            other => {
                return render_validation_error(
                    Some(&start.0),
                    &format!("Mode {other:?} is not a valid mode"),
                )
            }
        };
    }
    if let Some(v) = body.track_opens {
        server.track_opens = v;
    }
    if let Some(v) = body.track_clicks {
        server.track_clicks = v;
    }
    if body.spam_threshold.is_some() {
        server.spam_threshold = body.spam_threshold;
    }
    if body.outbound_spam_threshold.is_some() {
        server.outbound_spam_threshold = body.outbound_spam_threshold;
    }
    if body.bounce_hook_url.is_some() {
        server.bounce_hook_url = body.bounce_hook_url;
    }
    if body.delivery_hook_url.is_some() {
        server.delivery_hook_url = body.delivery_hook_url;
    }
    if body.inbound_domain.is_some() {
        server.inbound_domain = body.inbound_domain;
    }
    if body.broadcast_physical_address.is_some() {
        server.broadcast_physical_address = body.broadcast_physical_address;
    }
    if body.color.is_some() {
        server.color = body.color;
    }
    match state.store.update_server(server).await {
        Ok(server) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "server": server_json(&server) }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize)]
struct IpPoolAssignment {
    ip_pool_id: Option<u64>,
}

async fn servers_set_ip_pool(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
    Json(body): Json<IpPoolAssignment>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink).await {
        Ok(Some(server)) => {
            if let Err(error) = state
                .store
                .set_server_ip_pool(server.id, body.ip_pool_id)
                .await
            {
                return render_store_error(Some(&start.0), error);
            }
            match state
                .store
                .server_by_permalink(server.organization_id, &permalink)
                .await
            {
                Ok(Some(updated)) => render_success(
                    Some(&start.0),
                    StatusCode::OK,
                    json!({ "server": server_json(&updated) }),
                ),
                _ => render_success(
                    Some(&start.0),
                    StatusCode::OK,
                    json!({ "server": server_json(&server) }),
                ),
            }
        }
        Ok(None) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

// ------------------------------------------------------- admin API keys

fn admin_api_key_json(key: &camelmailer_core::AdminApiKey) -> Value {
    json!({
        "id": key.id,
        "uuid": key.uuid,
        "name": key.name,
        "key_prefix": key.key_prefix,
    })
}

async fn admin_api_keys_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    match state.store.list_admin_api_keys().await {
        Ok(keys) => {
            let result = paginate(&keys, &params);
            render_success(
                Some(&start.0),
                StatusCode::OK,
                json!({
                    "admin_api_keys": result.items.iter().map(admin_api_key_json).collect::<Vec<_>>(),
                    "pagination": result.pagination,
                }),
            )
        }
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize)]
struct CreateAdminApiKey {
    name: Option<String>,
}

async fn admin_api_keys_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Json(body): Json<CreateAdminApiKey>,
) -> ApiResponse {
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(
            Some(&start.0),
            "param is missing or the value is empty: name",
        );
    };
    let key = camelmailer_core::token::generate_key();
    match state.store.create_admin_api_key_record(&name, &key).await {
        Ok(record) => {
            // The one time the full secret is returned.
            let mut data = admin_api_key_json(&record);
            data["key"] = json!(key);
            render_success(
                Some(&start.0),
                StatusCode::CREATED,
                json!({ "admin_api_key": data }),
            )
        }
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

async fn admin_api_keys_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
) -> ApiResponse {
    match state.store.delete_admin_api_key(id).await {
        Ok(true) => render_deleted(Some(&start.0)),
        Ok(false) => render_not_found(Some(&start.0)),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

/// `GET /health` — unauthenticated liveness probe for load balancers and
/// container healthchecks. Deliberately outside both auth scopes and the
/// response envelope; it must never depend on storage.
async fn health() -> ApiResponse {
    ApiResponse {
        status: StatusCode::OK,
        body: json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }),
    }
}

pub(crate) fn permalink_from(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Build the `/api/v2/admin` router.
pub fn build_router(state: Arc<ApiState>) -> Router {
    let admin = Router::new()
        .route(
            "/organizations",
            get(organizations_index).post(organizations_create),
        )
        .route(
            "/organizations/{permalink}",
            get(organizations_show)
                .patch(organizations_update)
                .delete(organizations_destroy),
        )
        .route(
            "/organizations/{permalink}/servers",
            get(servers_index).post(servers_create),
        )
        .route(
            "/organizations/{permalink}/servers/stats",
            get(servers_stats),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}",
            get(servers_show)
                .patch(servers_update)
                .delete(servers_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/ip_pool",
            axum::routing::post(servers_set_ip_pool),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/stats",
            get(server_stats_show),
        )
        .route(
            "/admin_api_keys",
            get(admin_api_keys_index).post(admin_api_keys_create),
        )
        .route(
            "/admin_api_keys/{id}",
            axum::routing::delete(admin_api_keys_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/suspend",
            axum::routing::post(servers_suspend),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/unsuspend",
            axum::routing::post(servers_unsuspend),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/domains",
            get(resources::domains_index).post(resources::domains_create),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/domains/{name}",
            get(resources::domains_show).delete(resources::domains_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/domains/{name}/verify",
            axum::routing::post(resources::domains_verify),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/domains/{name}/health",
            get(resources::domains_health),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/credentials",
            get(resources::credentials_index).post(resources::credentials_create),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/credentials/{id}",
            get(resources::credentials_show)
                .patch(resources::credentials_update)
                .delete(resources::credentials_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/routes",
            get(resources::routes_index).post(resources::routes_create),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/routes/{id}",
            get(resources::routes_show)
                .patch(resources::routes_update)
                .delete(resources::routes_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/webhooks",
            get(resources::webhooks_index).post(resources::webhooks_create),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/webhooks/{id}",
            get(resources::webhooks_show)
                .patch(resources::webhooks_update)
                .delete(resources::webhooks_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/webhooks/{id}/enable",
            axum::routing::post(resources::webhooks_enable),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/webhooks/{id}/disable",
            axum::routing::post(resources::webhooks_disable),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/webhooks/{id}/test",
            axum::routing::post(resources::webhooks_test),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/sender_addresses",
            get(resources::sender_addresses_index).post(resources::sender_addresses_create),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/sender_addresses/{id}",
            axum::routing::delete(resources::sender_addresses_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/templates/{template_permalink}/copy_to",
            axum::routing::post(resources::templates_copy_to),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/suppressions",
            get(resources::suppressions_index).post(resources::suppressions_create),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/suppressions/{address}",
            axum::routing::delete(resources::suppressions_destroy),
        )
        .route(
            "/organizations/{permalink}/billing",
            get(crate::billing::billing_show),
        )
        .route(
            "/organizations/{permalink}/billing/portal",
            axum::routing::post(crate::billing::billing_portal),
        )
        .route(
            "/organizations/{permalink}/members",
            get(crate::memberships::members_index).post(crate::memberships::members_create),
        )
        .route(
            "/organizations/{permalink}/members/{user_id}",
            axum::routing::patch(crate::memberships::members_update)
                .delete(crate::memberships::members_destroy),
        )
        .route(
            "/organizations/{permalink}/invitations",
            get(crate::memberships::invitations_index).post(crate::memberships::invitations_create),
        )
        .route(
            "/organizations/{permalink}/invitations/{id}",
            axum::routing::delete(crate::memberships::invitations_destroy),
        )
        .route(
            "/organizations/{permalink}/sso/domains",
            get(crate::org_sso_api::sso_domains_index)
                .post(crate::org_sso_api::sso_domains_create),
        )
        .route(
            "/organizations/{permalink}/sso/domains/{id}/verify",
            axum::routing::post(crate::org_sso_api::sso_domains_verify),
        )
        .route(
            "/organizations/{permalink}/sso/domains/{id}",
            axum::routing::delete(crate::org_sso_api::sso_domains_destroy),
        )
        .route(
            "/organizations/{permalink}/sso/connections",
            get(crate::org_sso_api::sso_connections_index)
                .post(crate::org_sso_api::sso_connections_create),
        )
        .route(
            "/organizations/{permalink}/sso/connections/{id}",
            get(crate::org_sso_api::sso_connections_show)
                .patch(crate::org_sso_api::sso_connections_update)
                .delete(crate::org_sso_api::sso_connections_destroy),
        )
        .route("/auth_events", get(crate::memberships::auth_events_index))
        .route(
            "/users",
            get(resources::users_index).post(resources::users_create),
        )
        .route(
            "/users/{id}",
            get(resources::users_show)
                .patch(resources::users_update)
                .delete(resources::users_destroy),
        )
        .route(
            "/ip_pools",
            get(resources::ip_pools_index).post(resources::ip_pools_create),
        )
        .route(
            "/ip_pools/{id}",
            get(resources::ip_pools_show).delete(resources::ip_pools_destroy),
        )
        .route(
            "/ip_pools/{pool_id}/ip_addresses",
            get(resources::ip_addresses_index).post(resources::ip_addresses_create),
        )
        .route(
            "/ip_pools/{pool_id}/ip_addresses/{id}",
            get(resources::ip_addresses_show).delete(resources::ip_addresses_destroy),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn(timing_middleware))
        .with_state(state);

    Router::new()
        .route("/health", get(health))
        .nest("/api/v2/admin", admin)
}

/// Unauthenticated public asset serving. Layout logos are stored in Postgres
/// and referenced by mails as absolute URLs, so they must be fetchable without
/// a session or API key (like the tracking endpoints).
pub fn build_assets_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/assets/layouts/{uuid}/logo", get(serve_layout_logo))
        .with_state(state)
}

async fn serve_layout_logo(
    State(state): State<Arc<ApiState>>,
    Path(uuid): Path<String>,
) -> Response {
    let Some(store) = state.server_store.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match store.layout_logo(&uuid).await {
        Ok(Some((bytes, content_type))) => (
            [
                (axum::http::header::CONTENT_TYPE, content_type),
                (
                    axum::http::header::CACHE_CONTROL,
                    "public, max-age=86400".to_string(),
                ),
            ],
            bytes,
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
