//! The per-server API (`/api/v2/server/...`), authenticated by a server
//! token (`X-Server-API-Key`) rather than the account admin key. The token
//! resolves to exactly one server (a `credentials` record of type `API`);
//! every request is scoped to that server, and message-data queries enter
//! its RLS tenant context.
//!
//! This is a sibling router to the admin API — it is NOT layered under the
//! admin auth middleware.

use crate::app::{
    paginate, render_error, render_success, server_json, timing_middleware, ApiState,
    PaginationParams, RequestStart,
};
use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use camelmailer_core::mime::{self, Address, Attachment, BuildParams};
use camelmailer_core::{
    ActivityEvent, DeliveryRecord, MessageFilter, MessageRecord, MessageScope, QueuedMessage,
    Server, ServerContext,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// Resolve the `X-Server-API-Key` header to a server, reject suspended
/// servers, and inject the resolved `Server` (+ `ServerContext`) as request
/// extensions for the handlers.
async fn server_auth_middleware(
    State(state): State<Arc<ApiState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let start = request.extensions().get::<RequestStart>().copied();

    let key = request
        .headers()
        .get("X-Server-API-Key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    if key.is_empty() {
        return render_error(
            start.as_ref(),
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "Missing X-Server-API-Key header",
        )
        .into_response();
    }

    let server = match state.store.server_for_api_token(&key).await {
        Ok(Some(server)) => server,
        Ok(None) => {
            return render_error(
                start.as_ref(),
                StatusCode::UNAUTHORIZED,
                "Unauthorized",
                "Invalid server API token",
            )
            .into_response()
        }
        Err(_) => {
            return render_error(
                start.as_ref(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerError",
                "An internal error occurred",
            )
            .into_response()
        }
    };

    if server.suspended {
        return render_error(
            start.as_ref(),
            StatusCode::UNAUTHORIZED,
            "Unauthorized",
            "This server has been suspended",
        )
        .into_response();
    }

    request.extensions_mut().insert(ServerContext(server.id));
    request.extensions_mut().insert(server);
    next.run(request).await
}

/// `GET /api/v2/server` — the server the token is scoped to (proves scoping,
/// and doubles as Postmark's "get current server").
async fn server_show(
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    render_success(Some(&start.0), StatusCode::OK, json!({ "server": server_json(&server.0) }))
        .into_response()
}

/// `GET /api/v2/server/ping` — a liveness probe that confirms the token
/// resolved to the expected server.
async fn ping(
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "pong": true, "server_id": server.0.id, "server": server.0.permalink }),
    )
    .into_response()
}

// ----------------------------------------------------------------- send

#[derive(Debug, Deserialize)]
struct AddressInput {
    email: String,
    name: Option<String>,
}

impl From<AddressInput> for Address {
    fn from(a: AddressInput) -> Self {
        Address {
            name: a.name,
            email: a.email,
        }
    }
}

/// Accept either `"a@b.c"` or `{email, name}` for an address field.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum AddressOrString {
    String(String),
    Object(AddressInput),
}

impl From<AddressOrString> for Address {
    fn from(value: AddressOrString) -> Self {
        match value {
            AddressOrString::String(email) => Address { name: None, email },
            AddressOrString::Object(a) => a.into(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct AttachmentInput {
    name: String,
    content_type: String,
    data_base64: String,
}

#[derive(Debug, Deserialize, Default)]
struct SendMessage {
    from: Option<AddressOrString>,
    #[serde(default)]
    to: Vec<AddressOrString>,
    #[serde(default)]
    cc: Vec<AddressOrString>,
    #[serde(default)]
    bcc: Vec<AddressOrString>,
    #[serde(default)]
    reply_to: Vec<AddressOrString>,
    subject: Option<String>,
    html_body: Option<String>,
    text_body: Option<String>,
    #[serde(default)]
    headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    attachments: Vec<AttachmentInput>,
    tag: Option<String>,
    metadata: Option<Value>,
}

/// The From-address' domain, or an error message.
fn domain_of(address: &str) -> Option<&str> {
    address.rsplit_once('@').map(|(_, d)| d)
}

/// Validate + build one message and enqueue it per recipient. Returns the
/// per-message result object (native shape).
async fn enqueue_send(
    state: &ApiState,
    server: &Server,
    body: SendMessage,
) -> Result<Value, (StatusCode, String, String)> {
    let server_store = state.server_store.as_ref().ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalServerError".into(),
        "message storage is not configured".into(),
    ))?;

    let from: Address = body
        .from
        .map(Address::from)
        .filter(|a| !a.email.is_empty())
        .ok_or((
            StatusCode::BAD_REQUEST,
            "ParameterMissing".into(),
            "param is missing or the value is empty: from".into(),
        ))?;

    let to: Vec<Address> = body.to.into_iter().map(Address::from).collect();
    let cc: Vec<Address> = body.cc.into_iter().map(Address::from).collect();
    let bcc: Vec<Address> = body.bcc.into_iter().map(Address::from).collect();
    if to.is_empty() && cc.is_empty() && bcc.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "ParameterMissing".into(),
            "param is missing or the value is empty: to".into(),
        ));
    }

    // From-domain authorization: the server (or its org) must own a verified
    // domain matching the From address.
    let from_domain = domain_of(&from.email).ok_or((
        StatusCode::UNPROCESSABLE_ENTITY,
        "ValidationError".into(),
        "From address is not a valid email".into(),
    ))?;
    let domain_id = state
        .store
        .authenticated_domain(server.id, from_domain)
        .await
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerError".into(),
                "An internal error occurred".into(),
            )
        })?
        .ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError".into(),
            format!("From domain {from_domain:?} is not a verified sender for this server"),
        ))?;

    let attachments: Vec<Attachment> = body
        .attachments
        .into_iter()
        .map(|a| {
            let data = base64::engine::general_purpose::STANDARD
                .decode(a.data_base64.as_bytes())
                .map_err(|_| {
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "ValidationError".to_string(),
                        format!("attachment {:?} has invalid base64 data", a.name),
                    )
                })?;
            Ok(Attachment {
                filename: a.name,
                content_type: a.content_type,
                data,
            })
        })
        .collect::<Result<_, _>>()?;

    let params = BuildParams {
        from: from.clone(),
        to: to.clone(),
        cc: cc.clone(),
        bcc: bcc.clone(),
        reply_to: body.reply_to.into_iter().map(Address::from).collect(),
        subject: body.subject.unwrap_or_default(),
        html_body: body.html_body,
        text_body: body.text_body,
        headers: body.headers.into_iter().collect(),
        attachments,
        message_id: None,
    };
    let raw = mime::build_message(&params);

    // one stored message per recipient (matches SMTP intake semantics)
    let recipients: Vec<Address> = to.into_iter().chain(cc).chain(bcc).collect();
    let mut results = Vec::with_capacity(recipients.len());
    let mut shared_id: Option<i64> = None;
    for recipient in &recipients {
        let queued = QueuedMessage {
            server_id: server.id,
            rcpt_to: recipient.email.clone(),
            mail_from: from.email.clone(),
            raw_message: raw.clone(),
            received_with_ssl: false,
            scope: MessageScope::Outgoing,
            bounce: false,
            domain_id: Some(domain_id),
            credential_id: None,
            route_id: None,
            tag: body.tag.clone(),
            metadata: body.metadata.clone(),
        };
        match server_store.store_outgoing(queued).await {
            Ok(sent) => {
                shared_id.get_or_insert(sent.id);
                results.push(json!({
                    "rcpt_to": sent.rcpt_to,
                    "message_id": sent.id,
                    "token": sent.token,
                    "status": "queued",
                }));
            }
            Err(error) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "InternalServerError".into(),
                    error.to_string(),
                ))
            }
        }
    }

    Ok(json!({
        "message_id": shared_id,
        "recipients": results,
    }))
}

async fn messages_send(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(body): Json<SendMessage>,
) -> Response {
    match enqueue_send(&state, &server.0, body).await {
        Ok(data) => {
            render_success(Some(&start.0), StatusCode::CREATED, data).into_response()
        }
        Err((status, code, message)) => {
            render_error(Some(&start.0), status, &code, &message).into_response()
        }
    }
}

async fn messages_send_batch(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(messages): Json<Vec<SendMessage>>,
) -> Response {
    let mut results = Vec::with_capacity(messages.len());
    for message in messages {
        match enqueue_send(&state, &server.0, message).await {
            Ok(data) => results.push(json!({ "status": "success", "data": data })),
            Err((_, code, message)) => {
                results.push(json!({ "status": "error", "error": { "code": code, "message": message } }))
            }
        }
    }
    render_success(Some(&start.0), StatusCode::OK, json!({ "messages": results }))
        .into_response()
}

// -------------------------------------------------------------- read APIs

fn message_json(message: &MessageRecord) -> Value {
    json!({
        "id": message.id,
        "token": message.token,
        "scope": message.scope,
        "rcpt_to": message.rcpt_to,
        "mail_from": message.mail_from,
        "subject": message.subject,
        "message_id": message.message_id_header,
        "tag": message.tag,
        "status": message.status,
        "bounce": message.bounce,
        "spam_status": message.spam_status,
        "spam_score": message.spam_score,
        "held": message.held,
        "threat": message.threat,
        "size": message.size,
        "metadata": message.metadata,
        "created_at": message.created_at.to_rfc3339(),
    })
}

fn delivery_json(delivery: &DeliveryRecord) -> Value {
    json!({
        "id": delivery.id,
        "status": delivery.status,
        "details": delivery.details,
        "output": delivery.output,
        "sent_with_ssl": delivery.sent_with_ssl,
        "created_at": delivery.created_at.to_rfc3339(),
    })
}

fn activity_json(event: &ActivityEvent) -> Value {
    json!({
        "ip_address": event.ip_address,
        "user_agent": event.user_agent,
        "url": event.url,
        "created_at": event.created_at.to_rfc3339(),
    })
}

/// Query params for `GET /messages`: pagination plus filters.
#[derive(Debug, Deserialize, Default)]
struct MessageListParams {
    page: Option<u64>,
    per_page: Option<u64>,
    scope: Option<String>,
    status: Option<String>,
    tag: Option<String>,
    query: Option<String>,
}

/// The configured tenant-scoped store, if the API was built with one.
fn server_store(state: &ApiState) -> Option<&Arc<dyn camelmailer_core::ServerStore>> {
    state.server_store.as_ref()
}

fn storage_unconfigured(start: &RequestStart) -> Response {
    render_error(
        Some(start),
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalServerError",
        "message storage is not configured",
    )
    .into_response()
}

/// `GET /api/v2/server/messages` — the server's messages, filtered + paged.
async fn messages_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Query(params): Query<MessageListParams>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let filter = MessageFilter {
        scope: params.scope.filter(|s| !s.is_empty()),
        status: params.status.filter(|s| !s.is_empty()),
        tag: params.tag.filter(|s| !s.is_empty()),
        query: params.query.filter(|s| !s.is_empty()),
    };
    let messages = match store.messages(server.0.id, &filter).await {
        Ok(messages) => messages,
        Err(error) => {
            return render_error(
                Some(&start.0),
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerError",
                &error.to_string(),
            )
            .into_response()
        }
    };
    let pagination = PaginationParams {
        page: params.page,
        per_page: params.per_page,
    };
    let result = paginate(&messages, &pagination);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "messages": result.items.iter().map(message_json).collect::<Vec<_>>(),
            "pagination": result.pagination,
        }),
    )
    .into_response()
}

/// `GET /api/v2/server/messages/{id}` — one message plus its deliveries.
async fn message_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let message = match store.message(server.0.id, id).await {
        Ok(Some(message)) => message,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let deliveries = store
        .deliveries(server.0.id, id)
        .await
        .unwrap_or_default();
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "message": message_json(&message),
            "deliveries": deliveries.iter().map(delivery_json).collect::<Vec<_>>(),
        }),
    )
    .into_response()
}

/// `GET /api/v2/server/messages/{id}/deliveries`.
async fn message_deliveries(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    if !message_exists(store, server.0.id, id).await {
        return not_found(&start.0);
    }
    match store.deliveries(server.0.id, id).await {
        Ok(deliveries) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "deliveries": deliveries.iter().map(delivery_json).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/messages/{id}/opens`.
async fn message_opens(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    if !message_exists(store, server.0.id, id).await {
        return not_found(&start.0);
    }
    match store.opens(server.0.id, id).await {
        Ok(opens) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "opens": opens.iter().map(activity_json).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/messages/{id}/clicks`.
async fn message_clicks(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    if !message_exists(store, server.0.id, id).await {
        return not_found(&start.0);
    }
    match store.clicks(server.0.id, id).await {
        Ok(clicks) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "clicks": clicks.iter().map(activity_json).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/messages/{id}/raw` — the raw MIME, base64-encoded.
/// Returns 404 when the server is in privacy mode (raw content withheld).
async fn message_raw(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let message = match store.message(server.0.id, id).await {
        Ok(Some(message)) => message,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if server.0.privacy_mode {
        return render_error(
            Some(&start.0),
            StatusCode::NOT_FOUND,
            "NotAvailable",
            "Raw message content is not retained in privacy mode",
        )
        .into_response();
    }
    let raw = base64::engine::general_purpose::STANDARD.encode(&message.raw_message);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "raw_message": raw }),
    )
    .into_response()
}

async fn message_exists(
    store: &Arc<dyn camelmailer_core::ServerStore>,
    server_id: camelmailer_core::Id,
    id: i64,
) -> bool {
    matches!(store.message(server_id, id).await, Ok(Some(_)))
}

fn not_found(start: &RequestStart) -> Response {
    render_error(Some(start), StatusCode::NOT_FOUND, "NotFound", "Resource not found")
        .into_response()
}

fn internal_error(start: &RequestStart, message: &str) -> Response {
    render_error(
        Some(start),
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalServerError",
        message,
    )
    .into_response()
}

/// Build the `/api/v2/server` router (server-token authenticated).
pub fn build_server_router(state: Arc<ApiState>) -> Router {
    let server = Router::new()
        .route("/", get(server_show))
        .route("/ping", get(ping))
        .route("/messages", get(messages_index).post(messages_send))
        .route("/messages/batch", post(messages_send_batch))
        .route("/messages/{id}", get(message_show))
        .route("/messages/{id}/deliveries", get(message_deliveries))
        .route("/messages/{id}/opens", get(message_opens))
        .route("/messages/{id}/clicks", get(message_clicks))
        .route("/messages/{id}/raw", get(message_raw))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            server_auth_middleware,
        ))
        .layer(middleware::from_fn(timing_middleware))
        .with_state(state);

    Router::new().nest("/api/v2/server", server)
}
