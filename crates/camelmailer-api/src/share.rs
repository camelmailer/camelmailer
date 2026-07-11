//! Public message share links (`/api/v2/share/...`) — the unauthenticated
//! counterpart of `POST /api/v2/server/messages/{id}/share`.
//!
//! A share token is random, shown exactly once at creation, and stored
//! only as a SHA-256 hash. The public endpoint resolves the presented
//! token by hash (a cross-tenant lookup like tracking tokens), checks
//! expiry, and then reads the message through the normal tenant-scoped
//! storage under the resolved server context. Unknown tokens answer
//! `404 NotFound`; expired ones `404 ShareLinkExpired`.

use crate::app::{render_error, render_success, timing_middleware, ApiState, RequestStart};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use serde_json::json;
use std::sync::Arc;

/// How long a share link lives when the caller does not say (hours).
pub const DEFAULT_SHARE_EXPIRY_HOURS: i64 = 48;
/// The longest allowed share-link lifetime (hours; 7 days).
pub const MAX_SHARE_EXPIRY_HOURS: i64 = 168;

/// `GET /api/v2/share/messages/{token}` — the shared message with its
/// full support context: deliveries, opens, clicks and the decoded
/// display bodies. That is the point of a share link (support triage),
/// so the bodies are deliberately included.
async fn share_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(token): Path<String>,
) -> Response {
    let Some(store) = state.server_store.as_ref() else {
        return render_error(
            Some(&start.0),
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalServerError",
            "message storage is not configured",
        )
        .into_response();
    };

    let not_found = || {
        render_error(
            Some(&start.0),
            StatusCode::NOT_FOUND,
            "NotFound",
            "Resource not found",
        )
        .into_response()
    };

    let token_hash = camelmailer_core::auth::hash_token(&token);
    let share = match store.message_share_by_token_hash(&token_hash).await {
        Ok(Some(share)) => share,
        Ok(None) => return not_found(),
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
    if share.expires_at <= chrono::Utc::now() {
        return render_error(
            Some(&start.0),
            StatusCode::NOT_FOUND,
            "ShareLinkExpired",
            "This share link has expired",
        )
        .into_response();
    }

    let message = match store.message(share.server_id, share.message_id).await {
        Ok(Some(message)) => message,
        // the message is gone — the link is as good as unknown
        Ok(None) => return not_found(),
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
    let deliveries = store
        .deliveries(share.server_id, share.message_id)
        .await
        .unwrap_or_default();
    let opens = store
        .opens(share.server_id, share.message_id)
        .await
        .unwrap_or_default();
    let clicks = store
        .clicks(share.server_id, share.message_id)
        .await
        .unwrap_or_default();
    let bodies = camelmailer_core::mime::extract_bodies(&message.raw_message);

    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "message": crate::server_api::message_json(&message),
            "deliveries": deliveries
                .iter()
                .map(crate::server_api::delivery_json)
                .collect::<Vec<_>>(),
            "opens": opens
                .iter()
                .map(crate::server_api::activity_json)
                .collect::<Vec<_>>(),
            "clicks": clicks
                .iter()
                .map(crate::server_api::activity_json)
                .collect::<Vec<_>>(),
            "html_body": bodies.html,
            "text_body": bodies.text,
            "expires_at": share.expires_at.to_rfc3339(),
        }),
    )
    .into_response()
}

/// Build the public `/api/v2/share` router (no auth — its own branch,
/// deliberately outside both authenticated surfaces).
pub fn build_share_router(state: Arc<ApiState>) -> Router {
    let share = Router::new()
        .route("/messages/{token}", get(share_show))
        .layer(middleware::from_fn(timing_middleware))
        .with_state(state);
    Router::new().nest("/api/v2/share", share)
}
