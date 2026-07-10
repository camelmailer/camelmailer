//! The per-server API (`/api/v2/server/...`), authenticated by a server
//! token (`X-Server-API-Key`) rather than the account admin key. The token
//! resolves to exactly one server (a `credentials` record of type `API`);
//! every request is scoped to that server, and message-data queries enter
//! its RLS tenant context.
//!
//! This is a sibling router to the admin API — it is NOT layered under the
//! admin auth middleware.

use crate::app::{
    render_error, render_success, server_json, timing_middleware, ApiState, RequestStart,
};
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use camelmailer_core::mime::{self, Address, Attachment, BuildParams};
use camelmailer_core::{MessageScope, QueuedMessage, Server, ServerContext};
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

/// Build the `/api/v2/server` router (server-token authenticated).
pub fn build_server_router(state: Arc<ApiState>) -> Router {
    let server = Router::new()
        .route("/", get(server_show))
        .route("/ping", get(ping))
        .route("/messages", post(messages_send))
        .route("/messages/batch", post(messages_send_batch))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            server_auth_middleware,
        ))
        .layer(middleware::from_fn(timing_middleware))
        .with_state(state);

    Router::new().nest("/api/v2/server", server)
}
