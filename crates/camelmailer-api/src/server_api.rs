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
use axum::extract::{DefaultBodyLimit, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use camelmailer_core::mime::{self, Address, Attachment, BuildParams};
use camelmailer_core::{
    ActivityEvent, Campaign, CampaignStats, DeliveryRecord, MessageFilter, MessageRecord,
    MessageScope, MessageStream, NewCampaign, NewStream, NewTemplate, QueuedMessage, Server,
    ServerContext, Template,
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
    // archived) or the server's default stream. Broadcast streams also carry
    // per-recipient one-click unsubscribe (see below); transactional and
    // inbound streams are unaffected.
    let (stream_id, is_broadcast, stream_label) = match &body.stream {
        Some(permalink) => match server_store.stream_by_permalink(server.id, permalink).await {
            Ok(Some(stream)) if !stream.archived => (
                Some(stream.id),
                stream.stream_type == "broadcast",
                stream.permalink,
            ),
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
        None => {
            let stream_id = server.default_stream_id;
            let (is_broadcast, stream_label) = match stream_id {
                Some(id) => server_store
                    .list_streams(server.id)
                    .await
                    .map_err(|_| internal_error())?
                    .into_iter()
                    .find(|s| s.id == id)
                    .map(|s| (s.stream_type == "broadcast", s.permalink))
                    .unwrap_or((false, String::new())),
                None => (false, String::new()),
            };
            (stream_id, is_broadcast, stream_label)
        }
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
    // Non-broadcast sends share one raw message (unchanged behaviour). Broadcast
    // sends get a per-recipient raw carrying that recipient's List-Unsubscribe
    // header, so each opt-out link is unique — built inside the loop below.
    let raw = if is_broadcast {
        None
    } else {
        Some(mime::build_message(&params))
    };
    // Per-server track domains take precedence over the installation-wide
    // `dns.track_domain` (a verified one must CNAME here, so the /track/*
    // endpoints still receive the hits).
    let track_host = state
        .store
        .effective_track_domain(server.id)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| state.config.dns.track_domain.clone());
    let track_base = format!("{}://{}", state.config.camelmailer.web_protocol, track_host);

    // one stored message per recipient (matches SMTP intake semantics)
    let recipients: Vec<Address> = to.into_iter().chain(cc).chain(bcc).collect();

    // Opt-in gate: a broadcast stream may only send to addresses that have
    // consented. Reject the whole request naming the first offender.
    // Transactional/inbound sends are unaffected.
    if is_broadcast {
        if let Some(stream_id) = stream_id {
            for recipient in &recipients {
                let subscribed = server_store
                    .is_subscribed(server.id, stream_id, &recipient.email)
                    .await
                    .map_err(|_| internal_error())?;
                if !subscribed {
                    return Err((
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "ValidationError".into(),
                        format!(
                            "{} has not opted in to the {stream_label} stream",
                            recipient.email
                        ),
                    ));
                }
            }
        }
    }

    let mut results = Vec::with_capacity(recipients.len());
    let mut shared_id: Option<i64> = None;
    for recipient in &recipients {
        let raw_message = if is_broadcast {
            // Register a one-click unsubscribe token for this recipient/stream
            // and bake the RFC 8058 headers into the stored raw.
            let token = server_store
                .create_unsubscribe_token(server.id, stream_id, &recipient.email)
                .await
                .map_err(|_| internal_error())?;
            let mut params = params.clone();
            params.headers.push((
                "List-Unsubscribe".into(),
                format!("<{track_base}/track/u/{token}>, <mailto:unsubscribe@{track_host}>",),
            ));
            params.headers.push((
                "List-Unsubscribe-Post".into(),
                "List-Unsubscribe=One-Click".into(),
            ));
            // CAN-SPAM compliance footer: a visible unsubscribe link (reusing
            // this recipient's one-click token) and, when configured, the
            // sender's physical postal address. Baked into the stored raw so it
            // travels with the message like the List-Unsubscribe header above.
            let unsubscribe_url = format!("{track_base}/track/u/{token}");
            let address = server.broadcast_physical_address.as_deref();
            let mut html_footer = format!(
                "<div style=\"margin-top:24px;padding-top:16px;\
                 border-top:1px solid #e5e5e5;font-family:Arial,sans-serif;\
                 font-size:12px;color:#8a8a8a;\">You are receiving this because \
                 you subscribed. <a href=\"{unsubscribe_url}\" \
                 style=\"color:#8a8a8a;\">Unsubscribe</a>."
            );
            if let Some(addr) = address {
                html_footer.push_str("<br>");
                html_footer.push_str(&html_escape(addr));
            }
            html_footer.push_str("</div>");
            params.html_body = Some(match params.html_body.take() {
                Some(body) => format!("{body}{html_footer}"),
                None => html_footer,
            });
            let mut text_footer = format!("\n\n--\nUnsubscribe: {unsubscribe_url}");
            if let Some(addr) = address {
                text_footer.push('\n');
                text_footer.push_str(addr);
            }
            params.text_body = Some(match params.text_body.take() {
                Some(body) => format!("{body}{text_footer}"),
                None => text_footer.trim_start().to_string(),
            });
            mime::build_message(&params)
        } else {
            raw.clone().expect("non-broadcast raw is built once")
        };
        let queued = QueuedMessage {
            server_id: server.id,
            rcpt_to: recipient.email.clone(),
            mail_from: from.email.clone(),
            raw_message,
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

pub(crate) fn message_json(message: &MessageRecord) -> Value {
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
        "campaign_id": message.campaign_id,
        "bypassed": message.bypassed,
        "created_at": message.created_at.to_rfc3339(),
    })
}

pub(crate) fn delivery_json(delivery: &DeliveryRecord) -> Value {
    json!({
        "id": delivery.id,
        "status": delivery.status,
        "details": delivery.details,
        "output": delivery.output,
        "sent_with_ssl": delivery.sent_with_ssl,
        "created_at": delivery.created_at.to_rfc3339(),
    })
}

pub(crate) fn activity_json(event: &ActivityEvent) -> Value {
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
    /// Restrict to messages produced by one broadcast campaign (by id).
    campaign_id: Option<i64>,
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

/// Minimal HTML escaping for values interpolated into the compliance footer.
fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
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
        campaign_id: params.campaign_id.map(|id| id as camelmailer_core::Id),
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

// --------------------------------------------------------- share links

#[derive(Debug, Deserialize, Default)]
struct CreateShare {
    expires_in_hours: Option<i64>,
}

/// `POST /api/v2/server/messages/{id}/share` — create a public share link
/// for one message (Resend-style). The token is random, returned exactly
/// once inside the URL, and stored only as a SHA-256 hash; the public
/// lookup goes through the hash. Default expiry 48 h, maximum 168 h.
async fn message_share_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<i64>,
    body: Option<Json<CreateShare>>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let expires_in_hours = body
        .and_then(|Json(b)| b.expires_in_hours)
        .unwrap_or(crate::share::DEFAULT_SHARE_EXPIRY_HOURS);
    if !(1..=crate::share::MAX_SHARE_EXPIRY_HOURS).contains(&expires_in_hours) {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &format!(
                "expires_in_hours must be between 1 and {}",
                crate::share::MAX_SHARE_EXPIRY_HOURS
            ),
        )
        .into_response();
    }
    // server-scoped: only a message of THIS server can be shared
    if !message_exists(store, server.0.id, id).await {
        return not_found(&start.0);
    }

    let token = camelmailer_core::auth::generate_auth_token();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(expires_in_hours);
    let share = match store
        .create_message_share(camelmailer_core::NewMessageShare {
            server_id: server.0.id,
            message_id: id,
            token_hash: camelmailer_core::auth::hash_token(&token),
            expires_at,
        })
        .await
    {
        Ok(share) => share,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };

    let base = state
        .config
        .auth
        .frontend_url
        .as_deref()
        .unwrap_or("")
        .trim_end_matches('/')
        .to_string();
    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({
            "url": format!("{base}/share/m/{token}"),
            "expires_at": share.expires_at.to_rfc3339(),
        }),
    )
    .into_response()
}

// ------------------------------------------------------------- insights

/// `GET /api/v2/server/messages/{id}/insights` — the deliverability rule
/// catalog evaluated for one message (see [`crate::insights`]).
async fn message_insights(
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
    let checks = crate::insights::evaluate(&state, &server.0, &message).await;
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "checks": checks.iter().map(|check| check.json()).collect::<Vec<_>>(),
        }),
    )
    .into_response()
}

// -------------------------------------------------------- stats + bounces

/// Query params for `GET /stats`: an optional `created_at` time window
/// plus an optional tag to scope every counter to.
#[derive(Debug, Deserialize, Default)]
pub(crate) struct StatsParams {
    pub(crate) from: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) to: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) tag: Option<String>,
}

pub(crate) fn stats_json(stats: &camelmailer_core::MessageStats) -> Value {
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
        campaign_id: None,
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
        campaign_id: None,
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
        "ip_pool_id": stream.ip_pool_id,
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
    /// IP pool the stream sources outbound mail from (`None` = the server's
    /// pool). Accepted as-is; the FK enforces it references a real pool.
    ip_pool_id: Option<camelmailer_core::Id>,
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
            ip_pool_id: body.ip_pool_id,
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

/// Distinguish an omitted key from an explicit `null` for a nullable update
/// field: absent → `None` (leave unchanged), `null` → `Some(None)` (clear),
/// value → `Some(Some(v))` (set). Serde otherwise collapses `null` to the
/// outer `None`, making "clear" impossible.
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Ok(Some(Option::deserialize(deserializer)?))
}

#[derive(Debug, Deserialize, Default)]
struct UpdateStream {
    name: Option<String>,
    stream_type: Option<String>,
    archived: Option<bool>,
    /// Present (value or `null`) sets/clears the stream's IP pool; omitted
    /// leaves it unchanged.
    #[serde(default, deserialize_with = "double_option")]
    ip_pool_id: Option<Option<camelmailer_core::Id>>,
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
    if let Some(ip_pool_id) = body.ip_pool_id {
        stream.ip_pool_id = ip_pool_id;
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

// ----------------------------------------------------------- subscribers

fn subscription_json(subscription: &camelmailer_core::Subscription) -> Value {
    json!({
        "id": subscription.id,
        "address": subscription.address,
        "status": subscription.status,
        "created_at": subscription.created_at,
    })
}

/// Resolve a stream by permalink or render a 404. Shared by the subscriber
/// endpoints, which are all scoped to one stream.
async fn resolve_stream(
    store: &Arc<dyn camelmailer_core::ServerStore>,
    server_id: camelmailer_core::Id,
    permalink: &str,
    start: &RequestStart,
) -> Result<MessageStream, Response> {
    match store.stream_by_permalink(server_id, permalink).await {
        Ok(Some(stream)) => Ok(stream),
        Ok(None) => Err(not_found(start)),
        Err(error) => Err(internal_error(start, &error.to_string())),
    }
}

/// `GET /api/v2/server/streams/{permalink}/subscribers`.
async fn subscribers_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    match store.list_subscriptions(server.0.id, stream.id).await {
        Ok(subscriptions) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({
                "subscribers": subscriptions.iter().map(subscription_json).collect::<Vec<_>>()
            }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

#[derive(Debug, Deserialize)]
struct CreateSubscriber {
    address: Option<String>,
    status: Option<String>,
}

/// `POST /api/v2/server/streams/{permalink}/subscribers`.
async fn subscribers_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<CreateSubscriber>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let Some(address) = body.address.filter(|a| !a.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: address",
        )
        .into_response();
    };
    let status = body.status.unwrap_or_else(|| "subscribed".into());
    if !matches!(status.as_str(), "subscribed" | "unsubscribed") {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &format!("Subscription status {status:?} is not valid"),
        )
        .into_response();
    }
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    match store
        .upsert_subscription(server.0.id, stream.id, &address, &status)
        .await
    {
        Ok(subscription) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({ "subscriber": subscription_json(&subscription) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `DELETE /api/v2/server/streams/{permalink}/subscribers/{address}`.
async fn subscribers_delete(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path((permalink, address)): Path<(String, String)>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    match store
        .remove_subscription(server.0.id, stream.id, &address)
        .await
    {
        Ok(removed) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "deleted": removed }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// Cap on recipients processed by one campaign-send request; subscribers
/// beyond this are reported as `skipped` (not queued).
const CAMPAIGN_RECIPIENT_CAP: usize = 1000;

#[derive(Debug, Deserialize)]
struct CampaignSend {
    /// Same shape as a normal send minus `to`; `from` is a bare address.
    from: Option<String>,
    subject: Option<String>,
    html_body: Option<String>,
    text_body: Option<String>,
    /// Optional template to render for every recipient (like a templated send).
    template: Option<String>,
    template_model: Option<Value>,
}

/// `POST /api/v2/server/streams/{permalink}/send` — send the same content to
/// every currently-subscribed address of a broadcast stream. Each recipient
/// goes through the identical broadcast send path as a one-off send (opt-in
/// gate, per-recipient unsubscribe token + `List-Unsubscribe`, CAN-SPAM
/// footer) by reusing [`enqueue_send`] once per address. Synchronous; capped
/// at [`CAMPAIGN_RECIPIENT_CAP`] recipients per request.
async fn campaign_send(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<CampaignSend>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    if stream.stream_type != "broadcast" {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            "campaigns can only be sent on a broadcast stream",
        )
        .into_response();
    }

    // Resolve the content once: an inline body, or a rendered template folded
    // over any inline fallbacks (same precedence as a templated send).
    let (subject, html_body, text_body) = match body.template.filter(|t| !t.is_empty()) {
        Some(permalink) => {
            let template = match store.template_by_permalink(server.0.id, &permalink).await {
                Ok(Some(template)) => template,
                Ok(None) => {
                    return render_error(
                        Some(&start.0),
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "ValidationError",
                        &format!("Message template {permalink:?} does not exist"),
                    )
                    .into_response()
                }
                Err(error) => return internal_error(&start.0, &error.to_string()),
            };
            let model = body.template_model.unwrap_or_else(|| json!({}));
            match render_templated(store, server.0.id, &template, &model).await {
                Ok((subject, html_body, text_body)) => (
                    subject.or(body.subject),
                    html_body.or(body.html_body),
                    text_body.or(body.text_body),
                ),
                Err((status, code, message)) => {
                    return render_error(Some(&start.0), status, &code, &message).into_response()
                }
            }
        }
        None => (body.subject, body.html_body, body.text_body),
    };

    let subscribed: Vec<String> = match store.list_subscriptions(server.0.id, stream.id).await {
        Ok(subscriptions) => subscriptions
            .into_iter()
            .filter(|s| s.status == "subscribed")
            .map(|s| s.address)
            .collect(),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let skipped = subscribed.len().saturating_sub(CAMPAIGN_RECIPIENT_CAP);

    let mut queued = 0u64;
    for address in subscribed.into_iter().take(CAMPAIGN_RECIPIENT_CAP) {
        // Build a fresh per-recipient send that reuses the broadcast path.
        let message = SendMessage {
            from: body.from.clone().map(AddressOrString::String),
            to: vec![AddressOrString::String(address)],
            subject: subject.clone(),
            html_body: html_body.clone(),
            text_body: text_body.clone(),
            stream: Some(permalink.clone()),
            ..Default::default()
        };
        match enqueue_send(&state, &server.0, message).await {
            Ok(_) => queued += 1,
            Err((status, code, message)) => {
                return render_error(Some(&start.0), status, &code, &message).into_response()
            }
        }
    }

    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({ "queued": queued, "skipped": skipped }),
    )
    .into_response()
}

#[derive(Debug, Deserialize)]
struct ImportSubscribers {
    #[serde(default)]
    addresses: Vec<String>,
}

/// `POST /api/v2/server/streams/{permalink}/subscribers/import` — bulk-upsert
/// addresses as `subscribed`, skipping blanks and duplicates within the
/// request. Returns how many were upserted and the subscriber count after.
async fn subscribers_import(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<ImportSubscribers>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    let mut seen = std::collections::HashSet::new();
    let mut added = 0u64;
    for address in &body.addresses {
        let address = address.trim();
        if address.is_empty() || !seen.insert(address.to_string()) {
            continue;
        }
        match store
            .upsert_subscription(server.0.id, stream.id, address, "subscribed")
            .await
        {
            Ok(_) => added += 1,
            Err(error) => return internal_error(&start.0, &error.to_string()),
        }
    }
    let total = match store.list_subscriptions(server.0.id, stream.id).await {
        Ok(subscriptions) => subscriptions.len(),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "added": added, "total": total }),
    )
    .into_response()
}

/// `POST /api/v2/server/streams/{permalink}/subscribers/{address}/complaint` —
/// record a manual spam complaint: stream-scoped `complaint` suppression plus
/// flipping the subscription to `unsubscribed`. Idempotent.
async fn subscriber_complaint(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path((permalink, address)): Path<(String, String)>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    match store
        .record_complaint(server.0.id, stream.id, &address)
        .await
    {
        Ok(subscription) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "subscriber": subscription_json(&subscription) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

// ------------------------------------------------------------ campaigns

/// How many recipients the async expansion processes between progress writes.
const CAMPAIGN_BATCH_SIZE: usize = 200;

fn campaign_json(campaign: &Campaign) -> Value {
    json!({
        "id": campaign.id,
        "stream_id": campaign.stream_id,
        "name": campaign.name,
        "subject": campaign.subject,
        "from": campaign.from_address,
        "html_body": campaign.html_body,
        "text_body": campaign.text_body,
        "status": campaign.status,
        "total": campaign.total,
        "sent": campaign.sent,
        "scheduled_at": campaign.scheduled_at,
        "created_at": campaign.created_at,
        "completed_at": campaign.completed_at,
    })
}

/// [`campaign_json`] enriched with the audience stream's `permalink`/`name` so
/// the server-level campaign endpoints can show the target without a second
/// lookup. `stream` is the campaign's resolved stream (may be absent if it was
/// archived away).
fn campaign_json_with_stream(campaign: &Campaign, stream: Option<&MessageStream>) -> Value {
    let mut value = campaign_json(campaign);
    let object = value.as_object_mut().expect("campaign_json is an object");
    object.insert(
        "stream".into(),
        json!({
            "permalink": stream.map(|s| s.permalink.clone()),
            "name": stream.map(|s| s.name.clone()),
        }),
    );
    value
}

fn campaign_stats_json(stats: &CampaignStats) -> Value {
    json!({
        "total": stats.total,
        "sent": stats.sent,
        "delivered": stats.delivered,
        "failed": stats.failed,
        "opened": stats.opened,
        "clicked": stats.clicked,
        "unsubscribed": stats.unsubscribed,
    })
}

#[derive(Debug, Deserialize)]
struct CreateCampaign {
    name: Option<String>,
    /// Bare From address (the broadcast path authorizes its domain/sender).
    from: Option<String>,
    subject: Option<String>,
    html_body: Option<String>,
    text_body: Option<String>,
}

/// Expand a broadcast campaign into one message per subscriber. Factored out
/// of [`campaigns_create`] so the handler can `tokio::spawn` it (production)
/// while a test can await it directly: it iterates the stream's currently
/// `subscribed` addresses in batches, sends each through the shared broadcast
/// path ([`enqueue_send`]) with the campaign's content, attributes the stored
/// message to the campaign, advances `sent` per batch, and finally marks the
/// campaign `sent` + `completed_at`.
///
/// Resilient by design: a single recipient whose send fails is logged and
/// skipped; only a fatal error (subscriber lookup) marks the campaign
/// `failed`. Owns its arguments so it is `'static` for the spawn.
pub(crate) async fn expand_campaign(
    state: Arc<ApiState>,
    server: Server,
    campaign: Campaign,
    permalink: String,
) {
    let Some(store) = state.server_store.clone() else {
        return;
    };
    let subscribed: Vec<String> = match store
        .list_subscriptions(server.id, campaign.stream_id)
        .await
    {
        Ok(subscriptions) => subscriptions
            .into_iter()
            .filter(|s| s.status == "subscribed")
            .map(|s| s.address)
            .collect(),
        Err(error) => {
            tracing::error!(%error, campaign_id = campaign.id, "campaign expansion could not list subscribers");
            let _ = store
                .set_campaign_progress(
                    server.id,
                    campaign.id,
                    0,
                    "failed",
                    Some(chrono::Utc::now()),
                )
                .await;
            return;
        }
    };

    let mut sent = 0i64;
    for batch in subscribed.chunks(CAMPAIGN_BATCH_SIZE) {
        for address in batch {
            let message = SendMessage {
                from: campaign.from_address.clone().map(AddressOrString::String),
                to: vec![AddressOrString::String(address.clone())],
                subject: campaign.subject.clone(),
                html_body: campaign.html_body.clone(),
                text_body: campaign.text_body.clone(),
                stream: Some(permalink.clone()),
                ..Default::default()
            };
            match enqueue_send(&state, &server, message).await {
                Ok(data) => {
                    if let Some(id) = data.get("message_id").and_then(Value::as_i64) {
                        if let Err(error) =
                            store.set_message_campaign(server.id, id, campaign.id).await
                        {
                            tracing::warn!(%error, campaign_id = campaign.id, message_id = id, "could not attribute message to campaign");
                        }
                    }
                    sent += 1;
                }
                Err((_, code, message)) => {
                    tracing::warn!(%code, %message, %address, campaign_id = campaign.id, "campaign recipient send failed; skipping");
                }
            }
        }
        // Advance progress after each batch so a poller sees the campaign move.
        let _ = store
            .set_campaign_progress(server.id, campaign.id, sent, "sending", None)
            .await;
    }

    if let Err(error) = store
        .set_campaign_progress(
            server.id,
            campaign.id,
            sent,
            "sent",
            Some(chrono::Utc::now()),
        )
        .await
    {
        tracing::error!(%error, campaign_id = campaign.id, "could not finalize campaign");
    }
}

/// `POST /api/v2/server/streams/{permalink}/campaigns` — create a campaign on
/// a broadcast stream and expand it asynchronously. The request records the
/// campaign (status `sending`, `total` = current subscriber count), spawns the
/// batch expansion, and returns the campaign immediately (201) without
/// blocking on the recipient list.
async fn campaigns_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<CreateCampaign>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    if stream.stream_type != "broadcast" {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            "campaigns can only be created on a broadcast stream",
        )
        .into_response();
    }
    let Some(from) = body.from.filter(|f| !f.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: from",
        )
        .into_response();
    };

    // Snapshot the subscriber count for the campaign total. The expansion
    // re-reads the list itself (it must not block this request).
    let total = match store.list_subscriptions(server.0.id, stream.id).await {
        Ok(subscriptions) => subscriptions
            .iter()
            .filter(|s| s.status == "subscribed")
            .count() as i64,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };

    let campaign = match store
        .create_campaign(NewCampaign {
            server_id: server.0.id,
            stream_id: stream.id,
            name: body.name.filter(|n| !n.is_empty()),
            subject: body.subject.clone(),
            from_address: Some(from),
            html_body: body.html_body.clone(),
            text_body: body.text_body.clone(),
            total,
            // The legacy stream-scoped route keeps its send-immediately
            // behaviour; the server-level route is the planning surface.
            status: "sending".into(),
            scheduled_at: None,
        })
        .await
    {
        Ok(campaign) => campaign,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };

    // Fire-and-forget expansion; the request returns right away.
    tokio::spawn(expand_campaign(
        state.clone(),
        server.0.clone(),
        campaign.clone(),
        permalink,
    ));

    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({ "campaign": campaign_json(&campaign) }),
    )
    .into_response()
}

/// `GET /api/v2/server/streams/{permalink}/campaigns` — the stream's
/// campaigns, newest first.
async fn campaigns_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let stream = match resolve_stream(store, server.0.id, &permalink, &start.0).await {
        Ok(stream) => stream,
        Err(response) => return response,
    };
    match store.list_campaigns(server.0.id, stream.id).await {
        Ok(campaigns) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "campaigns": campaigns.iter().map(campaign_json).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `GET /api/v2/server/streams/{permalink}/campaigns/{id}` — one campaign plus
/// its aggregated analytics.
async fn campaign_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path((permalink, id)): Path<(String, camelmailer_core::Id)>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    // Resolve the stream so an unknown permalink is a 404 like its siblings.
    if let Err(response) = resolve_stream(store, server.0.id, &permalink, &start.0).await {
        return response;
    }
    let campaign = match store.get_campaign(server.0.id, id).await {
        Ok(Some(campaign)) => campaign,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let stats = match store.campaign_stats(server.0.id, id).await {
        Ok(stats) => stats,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "campaign": campaign_json(&campaign),
            "stats": campaign_stats_json(&stats),
        }),
    )
    .into_response()
}

// -------------------------------------------- server-level campaigns (planning)

/// Statuses in which a campaign may still be edited, sent-now or canceled.
fn campaign_is_planned(status: &str) -> bool {
    matches!(status, "draft" | "scheduled")
}

/// Resolve the audience stream of a campaign by id (across the server's
/// streams), for enriching the JSON with the target's permalink/name.
async fn campaign_stream(
    store: &Arc<dyn camelmailer_core::ServerStore>,
    server_id: camelmailer_core::Id,
    stream_id: camelmailer_core::Id,
) -> Option<MessageStream> {
    store
        .list_streams(server_id)
        .await
        .ok()
        .and_then(|streams| streams.into_iter().find(|s| s.id == stream_id))
}

/// Current opted-in subscriber count of a stream (the campaign audience size).
async fn subscribed_count(
    store: &Arc<dyn camelmailer_core::ServerStore>,
    server_id: camelmailer_core::Id,
    stream_id: camelmailer_core::Id,
) -> Result<i64, StoreErrorResponse> {
    store
        .list_subscriptions(server_id, stream_id)
        .await
        .map(|subs| subs.iter().filter(|s| s.status == "subscribed").count() as i64)
        .map_err(StoreErrorResponse)
}

/// Wrapper so `?` can bubble a store error out of the small helpers above into
/// the handler's rendered 500.
struct StoreErrorResponse(camelmailer_core::StoreError);

#[derive(Debug, Deserialize)]
struct CreateServerCampaign {
    /// The audience: a broadcast stream permalink.
    stream: Option<String>,
    name: Option<String>,
    /// Bare From address (the broadcast path authorizes its domain/sender).
    from: Option<String>,
    subject: Option<String>,
    html_body: Option<String>,
    text_body: Option<String>,
    /// RFC 3339 send time; when present (and not `send_now`) the campaign is
    /// scheduled and the in-process scheduler sends it when due.
    scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Send immediately on create (overrides `scheduled_at`).
    #[serde(default)]
    send_now: bool,
}

/// `GET /api/v2/server/campaigns` — every campaign of the server (across
/// streams), newest first, each carrying its audience stream's permalink/name.
async fn server_campaigns_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let campaigns = match store.list_server_campaigns(server.0.id).await {
        Ok(campaigns) => campaigns,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    // Resolve stream metadata once and index it by id.
    let streams = store.list_streams(server.0.id).await.unwrap_or_default();
    let by_id: std::collections::HashMap<_, _> = streams.iter().map(|s| (s.id, s)).collect();
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "campaigns": campaigns
                .iter()
                .map(|c| campaign_json_with_stream(c, by_id.get(&c.stream_id).copied()))
                .collect::<Vec<_>>(),
        }),
    )
    .into_response()
}

/// `POST /api/v2/server/campaigns` — create a server-level campaign targeting a
/// broadcast stream. `send_now` sends immediately (status `sending`), a
/// `scheduled_at` schedules it (status `scheduled`), otherwise it is a `draft`.
async fn server_campaigns_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(body): Json<CreateServerCampaign>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let Some(permalink) = body.stream.filter(|s| !s.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: stream",
        )
        .into_response();
    };
    // The stream must exist and be broadcast (422 otherwise).
    let stream = match store.stream_by_permalink(server.0.id, &permalink).await {
        Ok(Some(stream)) => stream,
        Ok(None) => {
            return render_error(
                Some(&start.0),
                StatusCode::UNPROCESSABLE_ENTITY,
                "ValidationError",
                &format!("Message stream {permalink:?} does not exist"),
            )
            .into_response()
        }
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if stream.stream_type != "broadcast" {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            "campaigns can only target a broadcast stream",
        )
        .into_response();
    }
    let Some(from) = body.from.filter(|f| !f.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: from",
        )
        .into_response();
    };

    // Audience snapshot; also the `total` for a send-now campaign.
    let total = match subscribed_count(store, server.0.id, stream.id).await {
        Ok(total) => total,
        Err(StoreErrorResponse(error)) => return internal_error(&start.0, &error.to_string()),
    };

    // Pick the initial lifecycle state: send-now wins, then a schedule, else a
    // draft. Only send-now expands right away.
    let (status, scheduled_at) = if body.send_now {
        ("sending", None)
    } else if let Some(at) = body.scheduled_at {
        ("scheduled", Some(at))
    } else {
        ("draft", None)
    };

    let campaign = match store
        .create_campaign(NewCampaign {
            server_id: server.0.id,
            stream_id: stream.id,
            name: body.name.filter(|n| !n.is_empty()),
            subject: body.subject.clone(),
            from_address: Some(from),
            html_body: body.html_body.clone(),
            text_body: body.text_body.clone(),
            total,
            status: status.into(),
            scheduled_at,
        })
        .await
    {
        Ok(campaign) => campaign,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };

    // Only a send-now campaign expands on create; drafts/scheduled wait.
    if body.send_now {
        tokio::spawn(expand_campaign(
            state.clone(),
            server.0.clone(),
            campaign.clone(),
            permalink,
        ));
    }

    render_success(
        Some(&start.0),
        StatusCode::CREATED,
        json!({ "campaign": campaign_json_with_stream(&campaign, Some(&stream)) }),
    )
    .into_response()
}

/// `GET /api/v2/server/campaigns/{id}` — one campaign (+ audience stream) plus
/// its aggregated analytics.
async fn server_campaign_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<camelmailer_core::Id>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let campaign = match store.get_campaign(server.0.id, id).await {
        Ok(Some(campaign)) => campaign,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    let stream = campaign_stream(store, server.0.id, campaign.stream_id).await;
    let stats = match store.campaign_stats(server.0.id, id).await {
        Ok(stats) => stats,
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "campaign": campaign_json_with_stream(&campaign, stream.as_ref()),
            "stats": campaign_stats_json(&stats),
        }),
    )
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
struct UpdateCampaign {
    #[serde(default, deserialize_with = "double_option")]
    name: Option<Option<String>>,
    from: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    subject: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    html_body: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option")]
    text_body: Option<Option<String>>,
    /// Present (value or `null`) sets/clears the schedule; omitted leaves it.
    /// Setting a time moves a draft to `scheduled`; clearing it drops back to
    /// `draft`.
    #[serde(default, deserialize_with = "double_option")]
    scheduled_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
}

/// `PATCH /api/v2/server/campaigns/{id}` — edit a `draft`/`scheduled` campaign.
/// 422 once it is sending/sent/failed/canceled.
async fn server_campaign_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<camelmailer_core::Id>,
    Json(body): Json<UpdateCampaign>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let campaign = match store.get_campaign(server.0.id, id).await {
        Ok(Some(campaign)) => campaign,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if !campaign_is_planned(&campaign.status) {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &format!("a {} campaign can no longer be edited", campaign.status),
        )
        .into_response();
    }

    // A schedule change also moves the status between draft and scheduled.
    let status = match &body.scheduled_at {
        Some(Some(_)) => Some("scheduled".to_string()),
        Some(None) => Some("draft".to_string()),
        None => None,
    };
    let update = camelmailer_core::CampaignUpdate {
        name: body.name,
        subject: body.subject,
        from_address: body.from.filter(|f| !f.is_empty()).map(Some),
        html_body: body.html_body,
        text_body: body.text_body,
        scheduled_at: body.scheduled_at,
        status,
        total: None,
    };
    match store.update_campaign(server.0.id, id, update).await {
        Ok(Some(campaign)) => {
            let stream = campaign_stream(store, server.0.id, campaign.stream_id).await;
            render_success(
                Some(&start.0),
                StatusCode::OK,
                json!({ "campaign": campaign_json_with_stream(&campaign, stream.as_ref()) }),
            )
            .into_response()
        }
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `POST /api/v2/server/campaigns/{id}/send` — send a `draft`/`scheduled`
/// campaign now: re-snapshot `total`, flip to `sending`, spawn the expansion.
/// 422 otherwise.
async fn server_campaign_send(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<camelmailer_core::Id>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let campaign = match store.get_campaign(server.0.id, id).await {
        Ok(Some(campaign)) => campaign,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if !campaign_is_planned(&campaign.status) {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &format!("a {} campaign cannot be sent", campaign.status),
        )
        .into_response();
    }
    let stream = match campaign_stream(store, server.0.id, campaign.stream_id).await {
        Some(stream) => stream,
        None => return internal_error(&start.0, "the campaign's audience stream is missing"),
    };
    // Re-snapshot the audience and flip to `sending` before expanding.
    let total = match subscribed_count(store, server.0.id, stream.id).await {
        Ok(total) => total,
        Err(StoreErrorResponse(error)) => return internal_error(&start.0, &error.to_string()),
    };
    let update = camelmailer_core::CampaignUpdate {
        status: Some("sending".into()),
        total: Some(total),
        ..Default::default()
    };
    let campaign = match store.update_campaign(server.0.id, id, update).await {
        Ok(Some(campaign)) => campaign,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    tokio::spawn(expand_campaign(
        state.clone(),
        server.0.clone(),
        campaign.clone(),
        stream.permalink.clone(),
    ));
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({ "campaign": campaign_json_with_stream(&campaign, Some(&stream)) }),
    )
    .into_response()
}

/// `POST /api/v2/server/campaigns/{id}/cancel` — cancel a `draft`/`scheduled`
/// campaign (status `canceled`). 422 otherwise.
async fn server_campaign_cancel(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(id): Path<camelmailer_core::Id>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let campaign = match store.get_campaign(server.0.id, id).await {
        Ok(Some(campaign)) => campaign,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if !campaign_is_planned(&campaign.status) {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            &format!("a {} campaign cannot be canceled", campaign.status),
        )
        .into_response();
    }
    let update = camelmailer_core::CampaignUpdate {
        status: Some("canceled".into()),
        ..Default::default()
    };
    match store.update_campaign(server.0.id, id, update).await {
        Ok(Some(campaign)) => {
            let stream = campaign_stream(store, server.0.id, campaign.stream_id).await;
            render_success(
                Some(&start.0),
                StatusCode::OK,
                json!({ "campaign": campaign_json_with_stream(&campaign, stream.as_ref()) }),
            )
            .into_response()
        }
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// How often the scheduler wakes to look for due campaigns.
const SCHEDULER_TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// One scheduler pass: list every server (the `servers` table is plain config,
/// so this is a cross-tenant read), and for each atomically claim its due
/// `scheduled` campaigns (`claim_due_campaigns` flips them to `sending` inside
/// the server's tenant context, so two passes never double-send) and await the
/// shared [`expand_campaign`] for each. Resilient by design: a per-server or
/// per-campaign error is logged and the pass moves on; it never panics. Public
/// so the loop and the tests can drive a single deterministic pass.
pub async fn run_scheduler_tick(state: &Arc<ApiState>) {
    let Some(store) = state.server_store.clone() else {
        return;
    };
    let servers = match store.list_all_servers().await {
        Ok(servers) => servers,
        Err(error) => {
            tracing::warn!(%error, "campaign scheduler could not list servers");
            return;
        }
    };
    let now = chrono::Utc::now();
    for server in servers {
        let due = match store.claim_due_campaigns(server.id, now).await {
            Ok(due) => due,
            Err(error) => {
                tracing::warn!(%error, server_id = server.id, "could not claim due campaigns");
                continue;
            }
        };
        for campaign in due {
            // Resolve the audience stream permalink; skip (and mark failed) if
            // it has gone missing.
            let Some(stream) = campaign_stream(&store, server.id, campaign.stream_id).await else {
                tracing::warn!(
                    campaign_id = campaign.id,
                    "scheduled campaign has no stream; marking failed"
                );
                let _ = store
                    .set_campaign_progress(
                        server.id,
                        campaign.id,
                        0,
                        "failed",
                        Some(chrono::Utc::now()),
                    )
                    .await;
                continue;
            };
            expand_campaign(state.clone(), server.clone(), campaign, stream.permalink).await;
        }
    }
}

/// The in-process campaign scheduler. Runs inside the `web-server` process so a
/// local install auto-sends scheduled campaigns without a separate worker: it
/// drives [`run_scheduler_tick`] every [`SCHEDULER_TICK`].
pub async fn run_campaign_scheduler(state: Arc<ApiState>) {
    if state.server_store.is_none() {
        tracing::info!("campaign scheduler disabled: no tenant-scoped store");
        return;
    }
    tracing::info!("campaign scheduler started");
    loop {
        tokio::time::sleep(SCHEDULER_TICK).await;
        run_scheduler_tick(&state).await;
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
        "layout_id": template.layout_id,
    })
}

fn layout_json(layout: &camelmailer_core::Layout) -> Value {
    json!({
        "id": layout.id,
        "uuid": layout.uuid,
        "name": layout.name,
        "permalink": layout.permalink,
        "html_wrapper": layout.html_wrapper,
        "text_wrapper": layout.text_wrapper,
    })
}

// ------------------------------------------------------------- layouts

#[derive(Debug, Deserialize)]
struct CreateLayout {
    name: Option<String>,
    permalink: Option<String>,
    html_wrapper: Option<String>,
    text_wrapper: Option<String>,
}

/// The HTML wrapper must embed the body raw — `{{{ content }}}` or
/// `{{& content }}` — or every mail would show its own markup as text.
fn wrapper_error(start: &RequestStart) -> Response {
    render_error(
        Some(start),
        StatusCode::UNPROCESSABLE_ENTITY,
        "ValidationError",
        "html_wrapper must embed the body with {{{ content }}} (raw interpolation)",
    )
    .into_response()
}

/// `GET /api/v2/server/layouts`.
async fn layouts_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.list_layouts(server.0.id).await {
        Ok(layouts) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "layouts": layouts.iter().map(layout_json).collect::<Vec<_>>() }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `POST /api/v2/server/layouts`.
async fn layouts_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Json(body): Json<CreateLayout>,
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
    let Some(html_wrapper) = body.html_wrapper.filter(|w| !w.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: html_wrapper",
        )
        .into_response();
    };
    if !camelmailer_core::wrapper_has_raw_content(&html_wrapper) {
        return wrapper_error(&start.0);
    }
    let permalink = body
        .permalink
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| permalink_from(&name));

    match store
        .create_layout(camelmailer_core::NewLayout {
            server_id: server.0.id,
            name,
            permalink,
            html_wrapper,
            text_wrapper: body.text_wrapper.filter(|w| !w.is_empty()),
        })
        .await
    {
        Ok(layout) => render_success(
            Some(&start.0),
            StatusCode::CREATED,
            json!({ "layout": layout_json(&layout) }),
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

#[derive(Debug, Deserialize)]
struct LogoUpload {
    /// A base64 `data:` URL: `data:image/png;base64,…`.
    data_url: Option<String>,
}

/// `POST /api/v2/server/layouts/{permalink}/logo` — store a logo image in
/// Postgres and return the public URL that mails embed. Kept as a served
/// asset (not an inline data: URI) so the image survives in real clients.
async fn layout_logo_upload(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<LogoUpload>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let Some(data_url) = body.data_url.filter(|d| !d.is_empty()) else {
        return render_error(
            Some(&start.0),
            StatusCode::BAD_REQUEST,
            "ParameterMissing",
            "param is missing or the value is empty: data_url",
        )
        .into_response();
    };
    let Some((content_type, b64)) = data_url
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(";base64,"))
    else {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            "data_url must be a base64 data: URL (data:<type>;base64,…)",
        )
        .into_response();
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()) else {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            "data_url payload is not valid base64",
        )
        .into_response();
    };
    if bytes.len() > 1_048_576 {
        return render_error(
            Some(&start.0),
            StatusCode::UNPROCESSABLE_ENTITY,
            "ValidationError",
            "logo image exceeds the 1 MB limit",
        )
        .into_response();
    }
    let layout = match store.layout_by_permalink(server.0.id, &permalink).await {
        Ok(Some(layout)) => layout,
        Ok(None) => {
            return render_error(
                Some(&start.0),
                StatusCode::NOT_FOUND,
                "NotFound",
                &format!("Layout {permalink:?} does not exist"),
            )
            .into_response()
        }
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if let Err(error) = store
        .set_layout_logo(server.0.id, layout.id, bytes, content_type.to_string())
        .await
    {
        return internal_error(&start.0, &error.to_string());
    }
    let url = format!(
        "{}://{}/assets/layouts/{}/logo",
        state.config.camelmailer.web_protocol, state.config.camelmailer.web_hostname, layout.uuid
    );
    render_success(Some(&start.0), StatusCode::OK, json!({ "url": url })).into_response()
}

/// `GET /api/v2/server/layouts/{permalink}`.
async fn layout_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    match store.layout_by_permalink(server.0.id, &permalink).await {
        Ok(Some(layout)) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "layout": layout_json(&layout) }),
        )
        .into_response(),
        Ok(None) => not_found(&start.0),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

#[derive(Debug, Deserialize, Default)]
struct UpdateLayout {
    name: Option<String>,
    html_wrapper: Option<String>,
    text_wrapper: Option<String>,
}

/// `PATCH /api/v2/server/layouts/{permalink}`.
async fn layout_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
    Json(body): Json<UpdateLayout>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let mut layout = match store.layout_by_permalink(server.0.id, &permalink).await {
        Ok(Some(layout)) => layout,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    if let Some(name) = body.name.filter(|n| !n.is_empty()) {
        layout.name = name;
    }
    if let Some(html_wrapper) = body.html_wrapper.filter(|w| !w.is_empty()) {
        if !camelmailer_core::wrapper_has_raw_content(&html_wrapper) {
            return wrapper_error(&start.0);
        }
        layout.html_wrapper = html_wrapper;
    }
    if body.text_wrapper.is_some() {
        layout.text_wrapper = body.text_wrapper.filter(|w| !w.is_empty());
    }
    match store.update_layout(layout).await {
        Ok(layout) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "layout": layout_json(&layout) }),
        )
        .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
}

/// `DELETE /api/v2/server/layouts/{permalink}` — templates keep working
/// and simply lose the layout reference.
async fn layout_destroy(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    server: axum::Extension<Server>,
    Path(permalink): Path<String>,
) -> Response {
    let store = match server_store(&state) {
        Some(store) => store,
        None => return storage_unconfigured(&start.0),
    };
    let layout = match store.layout_by_permalink(server.0.id, &permalink).await {
        Ok(Some(layout)) => layout,
        Ok(None) => return not_found(&start.0),
        Err(error) => return internal_error(&start.0, &error.to_string()),
    };
    match store.delete_layout(server.0.id, layout.id).await {
        Ok(_) => render_success(Some(&start.0), StatusCode::OK, json!({ "deleted": true }))
            .into_response(),
        Err(error) => internal_error(&start.0, &error.to_string()),
    }
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
    /// Permalink of the layout to wrap this template in (optional).
    layout: Option<String>,
}

/// Resolve a layout permalink to its id, `Ok(None)` for an empty value.
async fn resolve_layout(
    store: &std::sync::Arc<dyn camelmailer_core::ServerStore>,
    server_id: camelmailer_core::Id,
    permalink: &str,
) -> Result<Option<camelmailer_core::Id>, String> {
    if permalink.is_empty() {
        return Ok(None);
    }
    match store.layout_by_permalink(server_id, permalink).await {
        Ok(Some(layout)) => Ok(Some(layout.id)),
        Ok(None) => Err(format!("Layout {permalink:?} does not exist")),
        Err(error) => Err(error.to_string()),
    }
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
    let layout_id =
        match resolve_layout(store, server.0.id, body.layout.as_deref().unwrap_or("")).await {
            Ok(layout_id) => layout_id,
            Err(message) => {
                return render_error(
                    Some(&start.0),
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ValidationError",
                    &message,
                )
                .into_response()
            }
        };

    match store
        .create_template(NewTemplate {
            server_id: server.0.id,
            name,
            permalink,
            subject: body.subject,
            html_body: body.html_body,
            text_body: body.text_body,
            layout_id,
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
    /// Layout permalink; an empty string detaches the layout, absent
    /// leaves it unchanged.
    layout: Option<String>,
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
    if let Some(layout) = body.layout {
        template.layout_id = match resolve_layout(store, server.0.id, &layout).await {
            Ok(layout_id) => layout_id,
            Err(message) => {
                return render_error(
                    Some(&start.0),
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "ValidationError",
                    &message,
                )
                .into_response()
            }
        };
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

/// Render a template and, when it references a layout, wrap the rendered
/// bodies in the layout's wrappers (the wrapper sees the same model plus
/// `content`). Used by the preview endpoint and templated sends alike, so
/// both always agree.
async fn render_templated(
    store: &std::sync::Arc<dyn camelmailer_core::ServerStore>,
    server_id: camelmailer_core::Id,
    template: &Template,
    model: &Value,
) -> Result<RenderedFields, ApiError> {
    let (subject, mut html_body, mut text_body) = render_template_fields(template, model)?;
    if let Some(layout_id) = template.layout_id {
        let layout = store
            .layout_by_id(server_id, layout_id)
            .await
            .map_err(|error| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "InternalServerError".to_string(),
                    error.to_string(),
                )
            })?;
        // A dangling reference (layout deleted concurrently) renders the
        // template bare rather than failing the send.
        if let Some(layout) = layout {
            let wrap = |wrapper: &str, content: &str| -> Result<String, ApiError> {
                camelmailer_core::render_in_layout(wrapper, model, content).map_err(|error| {
                    (
                        StatusCode::UNPROCESSABLE_ENTITY,
                        "ValidationError".to_string(),
                        format!("layout render failed: {error}"),
                    )
                })
            };
            if let Some(content) = html_body.as_deref() {
                html_body = Some(wrap(&layout.html_wrapper, content)?);
            }
            if let (Some(content), Some(wrapper)) =
                (text_body.as_deref(), layout.text_wrapper.as_deref())
            {
                text_body = Some(wrap(wrapper, content)?);
            }
        }
    }
    Ok((subject, html_body, text_body))
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
    match render_templated(store, server.0.id, &template, &model).await {
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
    let (subject, html_body, text_body) =
        render_templated(store, server.id, &template, &model).await?;
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
/// Maximum request-body size (bytes) accepted on the message-send routes.
/// Derived from `smtp_server.max_message_size` (MB): a message may itself be
/// that large, and it arrives base64-encoded (~4/3 inflation) inside a JSON
/// envelope, so allow a generous 2× plus 1 MiB of slack for the other fields.
/// Guards the send handlers against an oversized body exhausting memory.
fn send_body_limit(config: &camelmailer_config::Config) -> usize {
    let message_bytes = (config.smtp_server.max_message_size.max(1) as usize) * 1024 * 1024;
    message_bytes * 2 + 1024 * 1024
}

pub fn build_server_router(state: Arc<ApiState>) -> Router {
    // Cap the send routes' request body so an oversized base64/raw message
    // cannot exhaust memory (a 413 is returned when exceeded).
    let body_limit = DefaultBodyLimit::max(send_body_limit(&state.config));
    let server = Router::new()
        .route("/", get(server_show))
        .route("/ping", get(ping))
        .route(
            "/messages",
            get(messages_index).post(messages_send).layer(body_limit),
        )
        .route(
            "/messages/batch",
            post(messages_send_batch).layer(body_limit),
        )
        .route(
            "/messages/with_template",
            post(messages_send_with_template).layer(body_limit),
        )
        .route(
            "/messages/with_template/batch",
            post(messages_send_with_template_batch).layer(body_limit),
        )
        .route("/messages/{id}", get(message_show))
        .route("/messages/{id}/deliveries", get(message_deliveries))
        .route("/messages/{id}/opens", get(message_opens))
        .route("/messages/{id}/clicks", get(message_clicks))
        .route("/messages/{id}/raw", get(message_raw))
        .route("/messages/{id}/share", post(message_share_create))
        .route("/messages/{id}/insights", get(message_insights))
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
        .route("/streams/{permalink}/send", post(campaign_send))
        .route(
            "/streams/{permalink}/campaigns",
            get(campaigns_index).post(campaigns_create),
        )
        .route("/streams/{permalink}/campaigns/{id}", get(campaign_show))
        // Server-level campaigns (the first-class planning surface). The
        // stream-scoped routes above are kept for back-compat.
        .route(
            "/campaigns",
            get(server_campaigns_index).post(server_campaigns_create),
        )
        .route(
            "/campaigns/{id}",
            get(server_campaign_show).patch(server_campaign_update),
        )
        .route("/campaigns/{id}/send", post(server_campaign_send))
        .route("/campaigns/{id}/cancel", post(server_campaign_cancel))
        .route(
            "/streams/{permalink}/subscribers",
            get(subscribers_index).post(subscribers_create),
        )
        .route(
            "/streams/{permalink}/subscribers/import",
            post(subscribers_import),
        )
        .route(
            "/streams/{permalink}/subscribers/{address}",
            axum::routing::delete(subscribers_delete),
        )
        .route(
            "/streams/{permalink}/subscribers/{address}/complaint",
            post(subscriber_complaint),
        )
        .route("/inbound", get(inbound_index))
        .route("/inbound/{id}", get(inbound_show))
        .route("/inbound/{id}/bypass", post(inbound_bypass))
        .route("/inbound/{id}/retry", post(inbound_retry))
        .route("/templates", get(templates_index).post(templates_create))
        .route("/layouts", get(layouts_index).post(layouts_create))
        .route(
            "/layouts/{permalink}/logo",
            axum::routing::post(layout_logo_upload),
        )
        .route(
            "/layouts/{permalink}",
            get(layout_show).patch(layout_update).delete(layout_destroy),
        )
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
