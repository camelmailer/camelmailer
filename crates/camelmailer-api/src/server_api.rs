//! The per-server API (`/api/v2/server/...`), authenticated by a server
//! token (`X-Server-API-Key`) rather than the account admin key. The token
//! resolves to exactly one server (a `credentials` record of type `API`);
//! every request is scoped to that server, and message-data queries enter
//! its RLS tenant context.
//!
//! This is a sibling router to the admin API — it is NOT layered under the
//! admin auth middleware.

use crate::app::{
    paginate, permalink_from, render_error, render_success, server_json, timing_middleware,
    ApiState, PaginationParams, RequestStart,
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
    ActivityEvent, DeliveryRecord, MessageFilter, MessageRecord, MessageScope, MessageStream,
    NewStream, NewTemplate, QueuedMessage, Server, ServerContext, Template,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// A rendered error: HTTP status, native error code, and message.
type ApiError = (StatusCode, String, String);
/// Rendered (subject, html_body, text_body) fields of a template.
type RenderedFields = (Option<String>, Option<String>, Option<String>);

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
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "server": server_json(&server.0) }),
    )
    .into_response()
}

/// `GET /api/v2/server/ping` — a liveness probe that confirms the token
/// resolved to the expected server.
async fn ping(start: axum::Extension<RequestStart>, server: axum::Extension<Server>) -> Response {
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "pong": true, "server_id": server.0.id, "server": server.0.permalink }),
    )
    .into_response()
}

// ----------------------------------------------------------------- send

#[derive(Debug, Deserialize)]
pub(crate) struct AddressInput {
    pub(crate) email: String,
    pub(crate) name: Option<String>,
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
pub(crate) enum AddressOrString {
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
pub(crate) struct AttachmentInput {
    name: String,
    content_type: String,
    data_base64: String,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct SendMessage {
    pub(crate) from: Option<AddressOrString>,
    #[serde(default)]
    pub(crate) to: Vec<AddressOrString>,
    #[serde(default)]
    pub(crate) cc: Vec<AddressOrString>,
    #[serde(default)]
    pub(crate) bcc: Vec<AddressOrString>,
    #[serde(default)]
    pub(crate) reply_to: Vec<AddressOrString>,
    pub(crate) subject: Option<String>,
    pub(crate) html_body: Option<String>,
    pub(crate) text_body: Option<String>,
    #[serde(default)]
    pub(crate) headers: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(crate) attachments: Vec<AttachmentInput>,
    pub(crate) tag: Option<String>,
    pub(crate) metadata: Option<Value>,
    /// Message-stream permalink; defaults to the server's default stream.
    pub(crate) stream: Option<String>,
}

/// The From-address' domain, or an error message.
fn domain_of(address: &str) -> Option<&str> {
    address.rsplit_once('@').map(|(_, d)| d)
}

/// Validate + build one message and enqueue it per recipient. Returns the
/// per-message result object (native shape). Also the internal entry point
/// for platform mail (`crate::app_mailer`), so app mail takes exactly the
/// HTTP-send path: From-domain authorization, default stream, MIME build,
/// `ServerStore::store_outgoing`.
pub(crate) async fn enqueue_send(
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

    // From authorization: the server (or its org) must own a verified
    // domain matching the From address, OR the exact From address must be
    // a confirmed sender address of the server. SMTP submission applies
    // the same two-step rule in the session state machine.
    let from_domain = domain_of(&from.email).ok_or((
        StatusCode::UNPROCESSABLE_ENTITY,
        "ValidationError".into(),
        "From address is not a valid email".into(),
    ))?;
    let internal_error = || {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalServerError".to_string(),
            "An internal error occurred".to_string(),
        )
    };
    let domain_id = match state
        .store
        .authenticated_domain(server.id, from_domain)
        .await
        .map_err(|_| internal_error())?
    {
        Some(domain_id) => Some(domain_id),
        None => {
            let confirmed = state
                .store
                .confirmed_sender_address(server.id, &from.email)
                .await
                .map_err(|_| internal_error())?;
            if !confirmed {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ValidationError".into(),
                    format!("From domain {from_domain:?} is not a verified sender for this server"),
                ));
            }
            // authorized by the exact address; no DKIM domain to attach
            None
        }
    };

    // Resolve the target stream: an explicit permalink (must exist and not be
    // archived) or the server's default stream.
    let stream_id = match &body.stream {
        Some(permalink) => match server_store.stream_by_permalink(server.id, permalink).await {
            Ok(Some(stream)) if !stream.archived => Some(stream.id),
            Ok(Some(_)) => {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ValidationError".into(),
                    format!("Message stream {permalink:?} is archived"),
                ))
            }
            Ok(None) => {
                return Err((
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ValidationError".into(),
                    format!("Message stream {permalink:?} does not exist"),
                ))
            }
            Err(_) => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "InternalServerError".into(),
                    "An internal error occurred".into(),
                ))
            }
        },
        None => server.default_stream_id,
    };

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
            domain_id,
            credential_id: None,
            route_id: None,
            tag: body.tag.clone(),
            metadata: body.metadata.clone(),
            stream_id,
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
        Ok(data) => render_success(Some(&start.0), StatusCode::CREATED, data).into_response(),
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
            Err((_, code, message)) => results
                .push(json!({ "status": "error", "error": { "code": code, "message": message } })),
        }
    }
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "messages": results }),
    )
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
        "bounce_category": message.bounce_category,
        "spam_status": message.spam_status,
        "spam_score": message.spam_score,
        "held": message.held,
        "threat": message.threat,
        "size": message.size,
        "metadata": message.metadata,
        "stream_id": message.stream_id,
        "bypassed": message.bypassed,
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
    /// Restrict to one message stream (by permalink).
    stream: Option<String>,
}

/// Resolve an optional `?stream=` permalink to a stream id. `Ok(None)` means
/// "no stream filter"; `Err` is a rendered 404 for an unknown stream.
async fn resolve_stream_filter(
    store: &Arc<dyn camelmailer_core::ServerStore>,
    server_id: camelmailer_core::Id,
    permalink: &Option<String>,
    start: &RequestStart,
) -> Result<Option<camelmailer_core::Id>, Response> {
    match permalink.as_deref().filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(permalink) => match store.stream_by_permalink(server_id, permalink).await {
            Ok(Some(stream)) => Ok(Some(stream.id)),
            Ok(None) => Err(not_found(start)),
            Err(error) => Err(internal_error(start, &error.to_string())),
        },
    }
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
    let stream_id = match resolve_stream_filter(store, server.0.id, &params.stream, &start.0).await
    {
        Ok(stream_id) => stream_id,
        Err(response) => return response,
    };
    let filter = MessageFilter {
        scope: params.scope.filter(|s| !s.is_empty()),
        status: params.status.filter(|s| !s.is_empty()),
        tag: params.tag.filter(|s| !s.is_empty()),
        query: params.query.filter(|s| !s.is_empty()),
        stream_id,
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
    let deliveries = store.deliveries(server.0.id, id).await.unwrap_or_default();
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

// -------------------------------------------------------- stats + bounces

/// Query params for `GET /stats`: an optional `created_at` time window
/// plus an optional tag to scope every counter to.
#[derive(Debug, Deserialize, Default)]
struct StatsParams {
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
    tag: Option<String>,
}

fn stats_json(stats: &camelmailer_core::MessageStats) -> Value {
    json!({
        "total": stats.total,
        "incoming": stats.incoming,
        "outgoing": stats.outgoing,
        "sent": stats.sent,
        "held": stats.held,
        "soft_fail": stats.soft_fail,
        "hard_fail": stats.hard_fail,
        "bounced": stats.bounced,
        "pending": stats.pending,
        "opens": stats.opens,
        "unique_opens": stats.unique_opens,
        "clicks": stats.clicks,
        "unique_clicks": stats.unique_clicks,
        "bounces": {
            "hard": stats.bounces_hard,
            "soft": stats.bounces_soft,
            "undetermined": stats.bounces_undetermined,
        },
    })
}

/// `GET /api/v2/server/stats` — aggregate message + engagement counters.
async fn stats_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Query(params): Query<StatsParams>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let filter = camelmailer_core::StatsFilter {
        from: params.from,
        to: params.to,
        tag: params.tag.filter(|t| !t.is_empty()),
    };
    match store.message_stats(server.0.id, &filter).await {
        Ok(stats) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "stats": stats_json(&stats) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/stats/deliveries` — pending outbound queue depth.
async fn delivery_stats_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.delivery_stats(server.0.id).await {
        Ok(stats) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "queued": stats.queued,
                "domains": stats.domains.iter().map(|d| json!({
                    "domain": d.domain,
                    "queued": d.count,
                })).collect::<Vec<_>>(),
            }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/bounces` — bounced messages, filtered + paged.
async fn bounces_index(
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
        stream_id: None,
    };
    let bounces = match store.bounces(server.0.id, &filter).await {
        Ok(bounces) => bounces,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let pagination = PaginationParams {
        page: params.page,
        per_page: params.per_page,
    };
    let result = paginate(&bounces, &pagination);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "bounces": result.items.iter().map(message_json).collect::<Vec<_>>(),
            "pagination": result.pagination,
        }),
    )
    .into_response()
}

/// `GET /api/v2/server/bounces/{id}` — one bounced message.
async fn bounce_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.bounce(server.0.id, id).await {
        Ok(Some(bounce)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "bounce": message_json(&bounce) }),
        )
        .into_response(),
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

// ------------------------------------------------------------------ tags

/// How far back `GET /tags` looks, in days.
const TAG_WINDOW_DAYS: i64 = 30;

/// `GET /api/v2/server/tags` — the tags used by the server's messages in
/// the last 30 days, with counts, most used first. Tenant-scoped (RLS).
async fn tags_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let since = chrono::Utc::now() - chrono::Duration::days(TAG_WINDOW_DAYS);
    match store.tags(server.0.id, since).await {
        Ok(tags) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "tags": tags.iter().map(|t| json!({
                    "tag": t.tag,
                    "count": t.count,
                })).collect::<Vec<_>>(),
            }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

// ------------------------------------------------------- API request log

/// Log one authenticated request to the API request log (the Resend
/// `/logs` pattern): method, path without the query string, status code,
/// duration and a truncated user agent — never bodies, keys or query
/// strings. The write happens fire-and-forget on a background task, so it
/// can neither slow down nor fail the request.
async fn request_log_middleware(
    State(state): State<Arc<ApiState>>,
    request: Request,
    next: Next,
) -> Response {
    let start = request.extensions().get::<RequestStart>().copied();
    let context = request
        .extensions()
        .get::<camelmailer_core::ServerContext>()
        .copied();
    let method = request.method().to_string();
    // inside the nested router the URI is stripped of `/api/v2/server`;
    // OriginalUri restores the full request path
    let path = request
        .extensions()
        .get::<axum::extract::OriginalUri>()
        .map(|uri| uri.path().to_string())
        .unwrap_or_else(|| request.uri().path().to_string());
    let user_agent = request
        .headers()
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|ua| ua.chars().take(255).collect::<String>())
        .filter(|ua| !ua.is_empty());

    let response = next.run(request).await;

    if let (Some(camelmailer_core::ServerContext(server_id)), Some(store)) =
        (context, state.server_store.clone())
    {
        let entry = camelmailer_core::NewApiRequest {
            server_id,
            method,
            path,
            status_code: response.status().as_u16() as i32,
            duration_ms: start
                .map(|start| start.0.elapsed().as_millis().min(i64::MAX as u128) as i64)
                .unwrap_or(0),
            user_agent,
        };
        tokio::spawn(async move {
            if let Err(error) = store.record_api_request(entry).await {
                tracing::warn!(%error, "failed to write the API request log");
            }
        });
    }
    response
}

/// Query params for `GET /logs`: pagination plus status-class, method and
/// time-window filters.
#[derive(Debug, Deserialize, Default)]
struct LogsParams {
    page: Option<u64>,
    per_page: Option<u64>,
    /// Status-code class: `2xx`, `3xx`, `4xx` or `5xx`.
    status: Option<String>,
    method: Option<String>,
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
}

/// Parse a `status=` class filter (`"2xx"`, `"4xx"`, …; the bare digit is
/// accepted too). `None` = invalid.
fn parse_status_class(value: &str) -> Option<i32> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1xx" | "1" => Some(1),
        "2xx" | "2" => Some(2),
        "3xx" | "3" => Some(3),
        "4xx" | "4" => Some(4),
        "5xx" | "5" => Some(5),
        _ => None,
    }
}

fn api_request_json(request: &camelmailer_core::ApiRequestRecord) -> Value {
    json!({
        "id": request.id,
        "method": request.method,
        "path": request.path,
        "status_code": request.status_code,
        "duration_ms": request.duration_ms,
        "user_agent": request.user_agent,
        "created_at": request.created_at.to_rfc3339(),
    })
}

/// `GET /api/v2/server/logs` — the server's request log, filtered + paged,
/// newest first.
async fn logs_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Query(params): Query<LogsParams>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let status_class = match params.status.as_deref().filter(|s| !s.is_empty()) {
        None => None,
        Some(value) => match parse_status_class(value) {
            Some(class) => Some(class),
            None => {
                return render_error(
                    Some(&start.0),
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ValidationError",
                    &format!("Status class {value:?} is not valid (use 2xx, 3xx, 4xx or 5xx)"),
                )
                .into_response()
            }
        },
    };
    let filter = camelmailer_core::ApiRequestFilter {
        status_class,
        method: params.method.filter(|m| !m.is_empty()),
        from: params.from,
        to: params.to,
    };
    let requests = match store.api_requests(server.0.id, &filter).await {
        Ok(requests) => requests,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let pagination = PaginationParams {
        page: params.page,
        per_page: params.per_page,
    };
    let result = paginate(&requests, &pagination);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "requests": result.items.iter().map(api_request_json).collect::<Vec<_>>(),
            "pagination": result.pagination,
        }),
    )
    .into_response()
}

fn not_found(start: &RequestStart) -> Response {
    render_error(
        Some(start),
        StatusCode::NOT_FOUND,
        "NotFound",
        "Resource not found",
    )
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

// ---------------------------------------------------- inbound management

/// `GET /api/v2/server/inbound` — incoming messages, filtered + paged.
async fn inbound_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Query(params): Query<MessageListParams>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream_id = match resolve_stream_filter(store, server.0.id, &params.stream, &start.0).await
    {
        Ok(stream_id) => stream_id,
        Err(response) => return response,
    };
    let filter = MessageFilter {
        scope: None, // forced to incoming by the store
        status: params.status.filter(|s| !s.is_empty()),
        tag: params.tag.filter(|s| !s.is_empty()),
        query: params.query.filter(|s| !s.is_empty()),
        stream_id,
    };
    let messages = match store.inbound_messages(server.0.id, &filter).await {
        Ok(messages) => messages,
        Err(error) => return internal_error(&start.0, &error.to_string()),
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
            "inbound": result.items.iter().map(message_json).collect::<Vec<_>>(),
            "pagination": result.pagination,
        }),
    )
    .into_response()
}

/// `GET /api/v2/server/inbound/{id}` — one incoming message.
async fn inbound_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.inbound_message(server.0.id, id).await {
        Ok(Some(message)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "message": message_json(&message) }),
        )
        .into_response(),
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `POST /api/v2/server/inbound/{id}/bypass` — re-queue, bypassing rules.
async fn inbound_bypass(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    requeue(&state, &start.0, server.0.id, id, true).await
}

/// `POST /api/v2/server/inbound/{id}/retry` — re-queue for processing.
async fn inbound_retry(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    requeue(&state, &start.0, server.0.id, id, false).await
}

async fn requeue(
    state: &ApiState,
    start: &RequestStart,
    server_id: camelmailer_core::Id,
    id: i64,
    bypass: bool,
) -> Response {
    let store = match server_store(state) {
        Some(store) => store,
        None => return storage_unconfigured(start),
    };
    let result = if bypass {
        store.bypass_message(server_id, id).await
    } else {
        store.retry_message(server_id, id).await
    };
    match result {
        Ok(Some(message)) => render_success(
            Some(start),
            StatusCode::OK,
            json!({ "message": message_json(&message), "requeued": true }),
        )
        .into_response(),
        Ok(None) => not_found(start),
        Err(error) => internal_error(start, &error.to_string()),
    }
}

// ------------------------------------------------------- message streams

fn stream_json(stream: &MessageStream) -> Value {
    json!({
        "id": stream.id,
        "uuid": stream.uuid,
        "name": stream.name,
        "permalink": stream.permalink,
        "stream_type": stream.stream_type,
        "archived": stream.archived,
    })
}

fn valid_stream_type(stream_type: &str) -> bool {
    matches!(stream_type, "transactional" | "broadcast" | "inbound")
}

/// `GET /api/v2/server/streams`.
async fn streams_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.list_streams(server.0.id).await {
        Ok(streams) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "streams": streams.iter().map(stream_json).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct CreateStream {
    name: Option<String>,
    permalink: Option<String>,
    stream_type: Option<String>,
}

/// `POST /api/v2/server/streams`.
async fn streams_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(body): Json<CreateStream>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: name",
        )
        .into_response();
    };
    let stream_type = body.stream_type.unwrap_or_else(|| "transactional".into());
    if !valid_stream_type(&stream_type) {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &format!("Stream type {stream_type:?} is not valid"),
        )
        .into_response();
    }
    let permalink = body
        .permalink
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| permalink_from(&name));

    match store
        .create_stream(NewStream {
            server_id: server.0.id,
            name,
            permalink,
            stream_type,
        })
        .await
    {
        Ok(stream) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({ "stream": stream_json(&stream) }),
        )
        .into_response(),
        Err(camelmailer_core::StoreError::Conflict(message)) => render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &message,
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/streams/{permalink}`.
async fn stream_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.stream_by_permalink(server.0.id, &permalink).await {
        Ok(Some(stream)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "stream": stream_json(&stream) }),
        )
        .into_response(),
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

#[derive(Debug, Deserialize, Default)]
struct UpdateStream {
    name: Option<String>,
    stream_type: Option<String>,
    archived: Option<bool>,
}

/// `PATCH /api/v2/server/streams/{permalink}`.
async fn stream_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<UpdateStream>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let mut stream = match store.stream_by_permalink(server.0.id, &permalink).await {
        Ok(Some(stream)) => stream,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if let Some(name) = body.name.filter(|n| !n.is_empty()) {
        stream.name = name;
    }
    if let Some(stream_type) = body.stream_type {
        if !valid_stream_type(&stream_type) {
            return render_error(
                Some(&start.0),
                StatusCode::UNPROCESSABLE_ENTITY,
                "ValidationError",
                &format!("Stream type {stream_type:?} is not valid"),
            )
            .into_response();
        }
        stream.stream_type = stream_type;
    }
    if let Some(archived) = body.archived {
        stream.archived = archived;
    }
    match store.update_stream(stream).await {
        Ok(stream) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "stream": stream_json(&stream) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `POST /api/v2/server/streams/{permalink}/archive`.
async fn stream_archive(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let mut stream = match store.stream_by_permalink(server.0.id, &permalink).await {
        Ok(Some(stream)) => stream,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    stream.archived = true;
    match store.update_stream(stream).await {
        Ok(stream) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "stream": stream_json(&stream) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

// ------------------------------------------------------------ templates

fn template_json(template: &Template) -> Value {
    json!({
        "id": template.id,
        "uuid": template.uuid,
        "name": template.name,
        "permalink": template.permalink,
        "subject": template.subject,
        "html_body": template.html_body,
        "text_body": template.text_body,
        "archived": template.archived,
    })
}

/// `GET /api/v2/server/templates`.
async fn templates_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.list_templates(server.0.id).await {
        Ok(templates) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "templates": templates.iter().map(template_json).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct CreateTemplate {
    name: Option<String>,
    permalink: Option<String>,
    subject: Option<String>,
    html_body: Option<String>,
    text_body: Option<String>,
}

/// `POST /api/v2/server/templates`.
async fn templates_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(body): Json<CreateTemplate>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let Some(name) = body.name.filter(|n| !n.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: name",
        )
        .into_response();
    };
    let permalink = body
        .permalink
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| permalink_from(&name));

    match store
        .create_template(NewTemplate {
            server_id: server.0.id,
            name,
            permalink,
            subject: body.subject,
            html_body: body.html_body,
            text_body: body.text_body,
        })
        .await
    {
        Ok(template) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({ "template": template_json(&template) }),
        )
        .into_response(),
        Err(camelmailer_core::StoreError::Conflict(message)) => render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &message,
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/templates/{permalink}`.
async fn template_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.template_by_permalink(server.0.id, &permalink).await {
        Ok(Some(template)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "template": template_json(&template) }),
        )
        .into_response(),
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

#[derive(Debug, Deserialize, Default)]
struct UpdateTemplate {
    name: Option<String>,
    subject: Option<String>,
    html_body: Option<String>,
    text_body: Option<String>,
    archived: Option<bool>,
}

/// `PATCH /api/v2/server/templates/{permalink}`.
async fn template_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<UpdateTemplate>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let mut template = match store.template_by_permalink(server.0.id, &permalink).await {
        Ok(Some(template)) => template,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if let Some(name) = body.name.filter(|n| !n.is_empty()) {
        template.name = name;
    }
    if body.subject.is_some() {
        template.subject = body.subject;
    }
    if body.html_body.is_some() {
        template.html_body = body.html_body;
    }
    if body.text_body.is_some() {
        template.text_body = body.text_body;
    }
    if let Some(archived) = body.archived {
        template.archived = archived;
    }
    match store.update_template(template).await {
        Ok(template) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "template": template_json(&template) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `POST /api/v2/server/templates/{permalink}/archive`.
async fn template_archive(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let mut template = match store.template_by_permalink(server.0.id, &permalink).await {
        Ok(Some(template)) => template,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    template.archived = true;
    match store.update_template(template).await {
        Ok(template) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "template": template_json(&template) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

#[derive(Debug, Deserialize, Default)]
struct RenderBody {
    template_model: Option<Value>,
}

/// Render a template's subject/html/text against a model, or a rendered
/// error tuple (native shape).
fn render_template_fields(template: &Template, model: &Value) -> Result<RenderedFields, ApiError> {
    let render_field = |field: &Option<String>| -> Result<Option<String>, ApiError> {
        match field {
            Some(source) => camelmailer_core::render_template(source, model)
                .map(Some)
                .map_err(|error| {
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "ValidationError".to_string(),
                        format!("template render failed: {error}"),
                    )
                }),
            None => Ok(None),
        }
    };
    Ok((
        render_field(&template.subject)?,
        render_field(&template.html_body)?,
        render_field(&template.text_body)?,
    ))
}

/// `POST /api/v2/server/templates/{permalink}/render` — dry-run preview.
async fn template_render(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<RenderBody>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let template = match store.template_by_permalink(server.0.id, &permalink).await {
        Ok(Some(template)) => template,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let model = body.template_model.unwrap_or_else(|| json!({}));
    match render_template_fields(&template, &model) {
        Ok((subject, html_body, text_body)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "rendered": {
                    "subject": subject,
                    "html_body": html_body,
                    "text_body": text_body,
                }
            }),
        )
        .into_response(),
        Err((status, code, message)) => {
            render_error(Some(&start.0), status, &code, &message).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
struct SendWithTemplate {
    #[serde(flatten)]
    message: SendMessage,
    template: Option<String>,
    template_model: Option<Value>,
}

/// Load the named template, render it against the model, and fold the result
/// into a [`SendMessage`] ready for [`enqueue_send`].
async fn build_templated_message(
    state: &ApiState,
    server: &Server,
    body: SendWithTemplate,
) -> Result<SendMessage, (StatusCode, String, String)> {
    let store = server_store(state).ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "InternalServerError".into(),
        "message storage is not configured".into(),
    ))?;
    let permalink = body.template.filter(|t| !t.is_empty()).ok_or((
        StatusCode::BAD_REQUEST,
        "ParameterMissing".into(),
        "param is missing or the value is empty: template".into(),
    ))?;
    let template = store
        .template_by_permalink(server.id, &permalink)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalServerError".into(),
                error.to_string(),
            )
        })?
        .ok_or((
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError".into(),
            format!("Message template {permalink:?} does not exist"),
        ))?;
    let model = body.template_model.unwrap_or_else(|| json!({}));
    let (subject, html_body, text_body) = render_template_fields(&template, &model)?;
    let mut message = body.message;
    message.subject = subject.or(message.subject);
    message.html_body = html_body.or(message.html_body);
    message.text_body = text_body.or(message.text_body);
    Ok(message)
}

/// `POST /api/v2/server/messages/with_template`.
async fn messages_send_with_template(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(body): Json<SendWithTemplate>,
) -> Response {
    let message = match build_templated_message(&state, &server.0, body).await {
        Ok(message) => message,
        Err((status, code, message)) => {
            return render_error(Some(&start.0), status, &code, &message).into_response()
        }
    };
    match enqueue_send(&state, &server.0, message).await {
        Ok(data) => render_success(Some(&start.0), StatusCode::CREATED, data).into_response(),
        Err((status, code, message)) => {
            render_error(Some(&start.0), status, &code, &message).into_response()
        }
    }
}

/// `POST /api/v2/server/messages/with_template/batch`.
async fn messages_send_with_template_batch(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(messages): Json<Vec<SendWithTemplate>>,
) -> Response {
    let mut results = Vec::with_capacity(messages.len());
    for body in messages {
        let outcome = match build_templated_message(&state, &server.0, body).await {
            Ok(message) => enqueue_send(&state, &server.0, message).await,
            Err(error) => Err(error),
        };
        match outcome {
            Ok(data) => results.push(json!({ "status": "success", "data": data })),
            Err((_, code, message)) => results
                .push(json!({ "status": "error", "error": { "code": code, "message": message } })),
        }
    }
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "messages": results }),
    )
    .into_response()
}

// ------------------------------------------------------- DMARC monitoring

/// Query params shared by the DMARC endpoints: an optional domain and
/// report date-range window, plus pagination for the report list.
#[derive(Debug, Deserialize, Default)]
struct DmarcParams {
    domain: Option<String>,
    from: Option<chrono::DateTime<chrono::Utc>>,
    to: Option<chrono::DateTime<chrono::Utc>>,
    page: Option<u64>,
    per_page: Option<u64>,
}

impl DmarcParams {
    fn filter(&self) -> camelmailer_core::DmarcFilter {
        camelmailer_core::DmarcFilter {
            domain: self.domain.clone().filter(|d| !d.is_empty()),
            from: self.from,
            to: self.to,
        }
    }
}

fn dmarc_report_json(report: &camelmailer_core::DmarcReport) -> Value {
    json!({
        "id": report.id,
        "domain": report.domain,
        "org_name": report.org_name,
        "org_email": report.org_email,
        "report_id": report.report_id,
        "date_range_begin": report.date_range_begin.to_rfc3339(),
        "date_range_end": report.date_range_end.to_rfc3339(),
        "received_at": report.received_at.to_rfc3339(),
        "record_count": report.record_count,
    })
}

fn dmarc_record_json(record: &camelmailer_core::DmarcRecordRow) -> Value {
    json!({
        "id": record.id,
        "source_ip": record.source_ip,
        "count": record.count,
        "disposition": record.disposition,
        "dkim_result": record.dkim_result,
        "spf_result": record.spf_result,
        "dkim_aligned": record.dkim_aligned,
        "spf_aligned": record.spf_aligned,
        "header_from": record.header_from,
        "envelope_from": record.envelope_from,
    })
}

/// `GET /api/v2/server/dmarc/summary?domain=&from=&to=` — the compliance
/// summary over the stored aggregate-report rows (top 20 sources).
async fn dmarc_summary(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Query(params): Query<DmarcParams>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let records = match store.dmarc_records(server.0.id, &params.filter()).await {
        Ok(records) => records,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let summary = camelmailer_core::dmarc::summarize(&records);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "summary": {
                "total": summary.total,
                "pass": summary.pass,
                "fail": summary.fail,
                "pass_rate": summary.pass_rate,
                "by_source": summary.by_source.iter().map(|source| json!({
                    "source_ip": source.source_ip,
                    "count": source.count,
                    "spf_aligned_pct": source.spf_aligned_pct,
                    "dkim_aligned_pct": source.dkim_aligned_pct,
                    "disposition_counts": source.disposition_counts,
                })).collect::<Vec<_>>(),
                "by_disposition": summary.by_disposition,
            }
        }),
    )
    .into_response()
}

/// `GET /api/v2/server/dmarc/reports` — stored aggregate reports,
/// filtered + paged, newest report range first.
async fn dmarc_reports_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Query(params): Query<DmarcParams>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let reports = match store.dmarc_reports(server.0.id, &params.filter()).await {
        Ok(reports) => reports,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let pagination = PaginationParams {
        page: params.page,
        per_page: params.per_page,
    };
    let result = paginate(&reports, &pagination);
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "reports": result.items.iter().map(dmarc_report_json).collect::<Vec<_>>(),
            "pagination": result.pagination,
        }),
    )
    .into_response()
}

/// `GET /api/v2/server/dmarc/reports/{id}` — one report with its rows.
async fn dmarc_report_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.dmarc_report(server.0.id, id).await {
        Ok(Some((report, records))) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "report": dmarc_report_json(&report),
                "records": records.iter().map(dmarc_record_json).collect::<Vec<_>>(),
            }),
        )
        .into_response(),
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// Build the `/api/v2/server` router (server-token authenticated).
pub fn build_server_router(state: Arc<ApiState>) -> Router {
    let server = Router::new()
        .route("/", get(server_show))
        .route("/ping", get(ping))
        .route("/messages", get(messages_index).post(messages_send))
        .route("/messages/batch", post(messages_send_batch))
        .route("/messages/with_template", post(messages_send_with_template))
        .route(
            "/messages/with_template/batch",
            post(messages_send_with_template_batch),
        )
        .route("/messages/{id}", get(message_show))
        .route("/messages/{id}/deliveries", get(message_deliveries))
        .route("/messages/{id}/opens", get(message_opens))
        .route("/messages/{id}/clicks", get(message_clicks))
        .route("/messages/{id}/raw", get(message_raw))
        .route("/stats", get(stats_show))
        .route("/stats/deliveries", get(delivery_stats_show))
        .route("/tags", get(tags_index))
        .route("/logs", get(logs_index))
        .route("/bounces", get(bounces_index))
        .route("/bounces/{id}", get(bounce_show))
        .route("/streams", get(streams_index).post(streams_create))
        .route(
            "/streams/{permalink}",
            get(stream_show).patch(stream_update),
        )
        .route("/streams/{permalink}/archive", post(stream_archive))
        .route("/inbound", get(inbound_index))
        .route("/inbound/{id}", get(inbound_show))
        .route("/inbound/{id}/bypass", post(inbound_bypass))
        .route("/inbound/{id}/retry", post(inbound_retry))
        .route("/templates", get(templates_index).post(templates_create))
        .route(
            "/templates/{permalink}",
            get(template_show).patch(template_update),
        )
        .route("/templates/{permalink}/archive", post(template_archive))
        .route("/templates/{permalink}/render", post(template_render))
        .route("/dmarc/summary", get(dmarc_summary))
        .route("/dmarc/reports", get(dmarc_reports_index))
        .route("/dmarc/reports/{id}", get(dmarc_report_show))
        // innermost first: the request log runs inside auth, so only
        // authenticated requests (with a resolved ServerContext) are logged
        .layer(middleware::from_fn_with_state(
            state.clone(),
            request_log_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            server_auth_middleware,
        ))
        .layer(middleware::from_fn(timing_middleware))
        .with_state(state);

    Router::new().nest("/api/v2/server", server)
}
