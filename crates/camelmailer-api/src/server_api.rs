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
use axum::routing::get;
use axum::Router;
use camelmailer_core::{Server, ServerContext};
use serde_json::json;
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

/// Build the `/api/v2/server` router (server-token authenticated).
pub fn build_server_router(state: Arc<ApiState>) -> Router {
    let server = Router::new()
        .route("/", get(server_show))
        .route("/ping", get(ping))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            server_auth_middleware,
        ))
        .layer(middleware::from_fn(timing_middleware))
        .with_state(state);

    Router::new().nest("/api/v2/server", server)
}
