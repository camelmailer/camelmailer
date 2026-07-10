//! Public, unauthenticated click/open tracking endpoints.
//!
//! `GET /track/c/{token}` records a click and 302-redirects to the original
//! URL; `GET /track/o/{token}.gif` records an open and returns a 1×1 GIF.
//! Both resolve the token to its tenant via the cross-tenant lookup table
//! and record into the RLS-protected tables under that tenant context.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use camelmailer_core::TrackingStore;
use std::sync::Arc;

/// A 1×1 transparent GIF.
const PIXEL: &[u8] = &[
    0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xff, 0xff, 0xff, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b,
];

pub struct TrackingState {
    pub store: Arc<dyn TrackingStore>,
}

/// Client IP from the `X-Forwarded-For` header (the tracking endpoints sit
/// behind the load balancer / reverse proxy that terminates TLS).
fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(|value| value.trim().to_string())
        .unwrap_or_default()
}

fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

async fn track_click(
    State(state): State<Arc<TrackingState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.store.resolve_token(&token).await {
        Ok(Some(target)) if target.kind == "click" => {
            let url = target
                .target_url
                .clone()
                .unwrap_or_else(|| "/".to_string());
            let _ = state
                .store
                .record_click(&target, &client_ip(&headers), &user_agent(&headers))
                .await;
            (StatusCode::FOUND, [(header::LOCATION, url)]).into_response()
        }
        Ok(_) => StatusCode::NOT_FOUND.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

async fn track_open(
    State(state): State<Arc<TrackingState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    // token arrives as `<token>.gif`
    let token = token.strip_suffix(".gif").unwrap_or(&token);
    if let Ok(Some(target)) = state.store.resolve_token(token).await {
        if target.kind == "open" {
            let _ = state
                .store
                .record_open(&target, &client_ip(&headers), &user_agent(&headers))
                .await;
        }
    }
    // Always return the pixel — never leak whether a token was valid.
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/gif"),
            (header::CACHE_CONTROL, "no-store, no-cache, must-revalidate"),
        ],
        PIXEL,
    )
        .into_response()
}

pub fn tracking_router(state: Arc<TrackingState>) -> Router {
    Router::new()
        .route("/track/c/{token}", get(track_click))
        .route("/track/o/{token}", get(track_open))
        .with_state(state)
}
