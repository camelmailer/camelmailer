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

pub struct ApiState {
    /// Storage — in-memory for tests, PostgreSQL in production.
    pub store: Arc<dyn AdminStore>,
    /// Tenant-scoped storage for the per-server API (`/api/v2/server`).
    pub server_store: Option<Arc<dyn camelmailer_core::ServerStore>>,
    /// `camelmailer.admin_api_key` — the global fallback key.
    pub global_admin_api_key: Option<String>,
}

impl ApiState {
    pub fn new(store: Arc<dyn AdminStore>, global_admin_api_key: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            store,
            server_store: None,
            global_admin_api_key,
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
            global_admin_api_key,
        })
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

pub(crate) fn render_success(start: Option<&RequestStart>, status: StatusCode, data: Value) -> ApiResponse {
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
    request.extensions_mut().insert(RequestStart(Instant::now()));
    next.run(request).await
}

async fn auth_middleware(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    let start = request.extensions().get::<RequestStart>().copied();
    let key = request
        .headers()
        .get("X-Admin-API-Key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");

    if key.is_empty() {
        return render_error(
            start.as_ref(),
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "Missing X-Admin-API-Key header",
        )
        .into_response();
    }
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
        "color": server.color,
        "ip_pool_id": server.ip_pool_id,
        "default_stream_id": server.default_stream_id,
    })
}

// ---------------------------------------------------------- organizations

async fn organizations_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let mut organizations = match state.store.list_organizations().await {
        Ok(organizations) => organizations,
        Err(error) => return render_store_error(Some(&start.0), error),
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
    Json(body): Json<CreateOrganization>,
) -> ApiResponse {
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_parameter_missing(Some(&start.0), "param is missing or the value is empty: name");
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
        Ok(organization) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({ "organization": organization_json(&organization) }),
        ),
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
        return render_parameter_missing(Some(&start.0), "param is missing or the value is empty: name");
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
            match state.store.server_by_permalink(server.organization_id, &permalink).await {
                Ok(Some(updated)) => render_success(
                    Some(&start.0),
                    StatusCode::OK,
                    json!({ "server": server_json(&updated) }),
                ),
                _ => render_success(Some(&start.0), StatusCode::OK, json!({ "server": server_json(&server) })),
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
            render_success(Some(&start.0), StatusCode::CREATED, json!({ "admin_api_key": data }))
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
            get(organizations_show).delete(organizations_destroy),
        )
        .route(
            "/organizations/{permalink}/servers",
            get(servers_index).post(servers_create),
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
            get(resources::webhooks_show).delete(resources::webhooks_destroy),
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
            "/organizations/{permalink}/servers/{server_permalink}/suppressions",
            get(resources::suppressions_index).post(resources::suppressions_create),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/suppressions/{address}",
            axum::routing::delete(resources::suppressions_destroy),
        )
        .route("/users", get(resources::users_index).post(resources::users_create))
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

    Router::new().nest("/api/v2/admin", admin)
}
