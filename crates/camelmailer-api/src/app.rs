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
use camelmailer_core::{token, MemoryStore, Organization, Server, ServerMode};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Instant;
use subtle::ConstantTimeEq;

pub struct ApiState {
    pub store: Arc<MemoryStore>,
    /// Database-backed admin API keys (`AdminAPIKey` records).
    pub admin_api_keys: RwLock<HashSet<String>>,
    /// `camelmailer.admin_api_key` — the global fallback key.
    pub global_admin_api_key: Option<String>,
}

impl ApiState {
    pub fn new(store: Arc<MemoryStore>, global_admin_api_key: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            store,
            admin_api_keys: RwLock::new(HashSet::new()),
            global_admin_api_key,
        })
    }

    pub fn add_admin_api_key(&self, key: impl Into<String>) {
        self.admin_api_keys.write().unwrap().insert(key.into());
    }

    fn key_is_valid(&self, key: &str) -> bool {
        if self.admin_api_keys.read().unwrap().contains(key) {
            return true;
        }
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
struct RequestStart(Instant);

fn elapsed(request_start: Option<&RequestStart>) -> f64 {
    let seconds = request_start
        .map(|start| start.0.elapsed().as_secs_f64())
        .unwrap_or(0.0);
    (seconds * 1000.0).round() / 1000.0
}

struct ApiResponse {
    status: StatusCode,
    body: Value,
}

impl IntoResponse for ApiResponse {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

fn render_success(start: Option<&RequestStart>, status: StatusCode, data: Value) -> ApiResponse {
    ApiResponse {
        status,
        body: json!({
            "status": "success",
            "time": elapsed(start),
            "data": data,
        }),
    }
}

fn render_error(
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

fn render_deleted(start: Option<&RequestStart>) -> ApiResponse {
    render_success(start, StatusCode::OK, json!({ "deleted": true }))
}

fn render_not_found(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::NOT_FOUND,
        "NotFound",
        "Resource not found",
    )
}

fn render_validation_error(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(
        start,
        StatusCode::UNPROCESSABLE_ENTITY,
        "ValidationError",
        message,
    )
}

fn render_parameter_missing(start: Option<&RequestStart>, message: &str) -> ApiResponse {
    render_error(start, StatusCode::BAD_REQUEST, "ParameterMissing", message)
}

async fn timing_middleware(mut request: Request, next: Next) -> Response {
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
    if !state.key_is_valid(key) {
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
struct PaginationParams {
    page: Option<u64>,
    per_page: Option<u64>,
}

struct Paginated<T> {
    items: Vec<T>,
    pagination: Value,
}

fn paginate<T: Clone>(collection: &[T], params: &PaginationParams) -> Paginated<T> {
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

fn server_json(server: &Server) -> Value {
    json!({
        "id": server.id,
        "uuid": server.uuid,
        "name": server.name,
        "permalink": server.permalink,
        "mode": match server.mode { ServerMode::Live => "Live", ServerMode::Development => "Development" },
        "suspended": server.suspended,
        "suspension_reason": server.suspension_reason,
        "privacy_mode": server.privacy_mode,
    })
}

// ---------------------------------------------------------- organizations

async fn organizations_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let mut organizations = state.store.organizations();
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

    if state
        .store
        .organizations()
        .iter()
        .any(|o| o.permalink == permalink)
    {
        return render_validation_error(Some(&start.0), "Permalink has already been taken");
    }

    let organization = state.store.insert_organization(Organization {
        id: state.store.next_id(),
        uuid: token::generate_uuid(),
        name,
        permalink,
    });
    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({ "organization": organization_json(&organization) }),
    )
}

fn find_organization(state: &ApiState, permalink: &str) -> Option<Organization> {
    state
        .store
        .organizations()
        .into_iter()
        .find(|o| o.permalink == permalink)
}

async fn organizations_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    match find_organization(&state, &permalink) {
        Some(organization) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "organization": organization_json(&organization) }),
        ),
        None => render_not_found(Some(&start.0)),
    }
}

async fn organizations_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    match find_organization(&state, &permalink) {
        Some(organization) => {
            state.store.delete_organization(organization.id);
            render_deleted(Some(&start.0))
        }
        None => render_not_found(Some(&start.0)),
    }
}

// ---------------------------------------------------------------- servers

async fn servers_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
    Query(params): Query<PaginationParams>,
) -> ApiResponse {
    let Some(organization) = find_organization(&state, &org_permalink) else {
        return render_not_found(Some(&start.0));
    };
    let servers: Vec<Server> = state
        .store
        .servers()
        .into_iter()
        .filter(|s| s.organization_id == organization.id)
        .collect();
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
    let Some(organization) = find_organization(&state, &org_permalink) else {
        return render_not_found(Some(&start.0));
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

    if state
        .store
        .servers()
        .iter()
        .any(|s| s.organization_id == organization.id && s.permalink == permalink)
    {
        return render_validation_error(Some(&start.0), "Permalink has already been taken");
    }

    let server = state.store.insert_server(Server {
        id: state.store.next_id(),
        uuid: token::generate_uuid(),
        organization_id: organization.id,
        name,
        permalink,
        token: token::generate_token(6),
        mode,
        suspended: false,
        suspension_reason: None,
        privacy_mode: false,
        log_smtp_data: false,
        allow_sender: false,
    });
    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({ "server": server_json(&server) }),
    )
}

fn find_server(state: &ApiState, org_permalink: &str, permalink: &str) -> Option<Server> {
    let organization = find_organization(state, org_permalink)?;
    state
        .store
        .servers()
        .into_iter()
        .find(|s| s.organization_id == organization.id && s.permalink == permalink)
}

async fn servers_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink) {
        Some(server) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "server": server_json(&server) }),
        ),
        None => render_not_found(Some(&start.0)),
    }
}

async fn servers_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink) {
        Some(server) => {
            state.store.delete_server(server.id);
            render_deleted(Some(&start.0))
        }
        None => render_not_found(Some(&start.0)),
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
    match find_server(&state, &org_permalink, &permalink) {
        Some(mut server) => {
            server.suspended = true;
            server.suspension_reason = body
                .and_then(|Json(b)| b.reason)
                .or(Some("Suspended via Admin API".into()));
            let server = state.store.insert_server(server);
            render_success(
                Some(&start.0),
                StatusCode::OK,
                json!({ "server": server_json(&server) }),
            )
        }
        None => render_not_found(Some(&start.0)),
    }
}

async fn servers_unsuspend(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org_permalink, permalink)): Path<(String, String)>,
) -> ApiResponse {
    match find_server(&state, &org_permalink, &permalink) {
        Some(mut server) => {
            server.suspended = false;
            server.suspension_reason = None;
            let server = state.store.insert_server(server);
            render_success(
                Some(&start.0),
                StatusCode::OK,
                json!({ "server": server_json(&server) }),
            )
        }
        None => render_not_found(Some(&start.0)),
    }
}

fn permalink_from(name: &str) -> String {
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
            get(servers_show).delete(servers_destroy),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/suspend",
            axum::routing::post(servers_suspend),
        )
        .route(
            "/organizations/{permalink}/servers/{server_permalink}/unsuspend",
            axum::routing::post(servers_unsuspend),
        )
        .layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        .layer(middleware::from_fn(timing_middleware))
        .with_state(state);

    Router::new().nest("/api/v2/admin", admin)
}
