//! Public, unauthenticated click/open tracking endpoints.
//!
//! `GET /track/c/{token}` records a click and 302-redirects to the original
//! URL; `GET /track/o/{token}.gif` records an open and returns a 1×1 GIF.
//! Both resolve the token to its tenant via the cross-tenant lookup table
//! and record into the RLS-protected tables under that tenant context.

use crate::app::ApiState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
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

/// Longest User-Agent we store. The column is `TEXT` (unbounded in Postgres),
/// but a hostile client controls this header on the public, unauthenticated
/// tracking endpoints, so cap it defensively before it reaches the database.
const MAX_USER_AGENT_LEN: usize = 255;

fn user_agent(headers: &HeaderMap) -> String {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(MAX_USER_AGENT_LEN)
        .collect()
}

async fn track_click(
    State(state): State<Arc<TrackingState>>,
    Path(token): Path<String>,
    headers: HeaderMap,
) -> Response {
    match state.store.resolve_token(&token).await {
        Ok(Some(target)) if target.kind == "click" => {
            let url = target.target_url.clone().unwrap_or_else(|| "/".to_string());
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

/// Confirmation page shown after a browser (GET) unsubscribe. Deliberately
/// content-free about token validity — the same page is returned whether or
/// not the token matched.
const UNSUBSCRIBE_PAGE: &str =
    "<!doctype html><html><head><meta charset=\"utf-8\"><title>Unsubscribed</title></head>\
     <body><p>You have been unsubscribed.</p></body></html>";

/// One-click / browser unsubscribe. Both verbs resolve the token and, if it
/// matches, create a stream-scoped `unsubscribe` suppression (idempotent).
/// The result never leaks token validity: GET always returns the neutral
/// confirmation page, POST (RFC 8058 `List-Unsubscribe-Post`) an empty 200.
async fn unsubscribe(State(state): State<Arc<ApiState>>, Path(token): Path<String>) -> Response {
    if let Some(store) = state.server_store.as_ref() {
        let _ = store.record_unsubscribe(&token).await;
    }
    (StatusCode::OK, Html(UNSUBSCRIBE_PAGE)).into_response()
}

async fn unsubscribe_post(
    State(state): State<Arc<ApiState>>,
    Path(token): Path<String>,
) -> Response {
    if let Some(store) = state.server_store.as_ref() {
        let _ = store.record_unsubscribe(&token).await;
    }
    StatusCode::OK.into_response()
}

/// Public one-click unsubscribe endpoint (`GET`/`POST /track/u/{token}`).
/// Separate from [`tracking_router`] because it resolves through
/// [`camelmailer_core::ServerStore`] (which both the Postgres and in-memory
/// stores implement), so it is available regardless of the tracking backend.
pub fn unsubscribe_router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/track/u/{token}", get(unsubscribe).post(unsubscribe_post))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::{user_agent, MAX_USER_AGENT_LEN};
    use axum::http::{header, HeaderMap, HeaderValue};

    #[test]
    fn user_agent_is_truncated_to_a_sane_max() {
        let mut headers = HeaderMap::new();
        let long = "M".repeat(10_000);
        headers.insert(header::USER_AGENT, HeaderValue::from_str(&long).unwrap());
        let captured = user_agent(&headers);
        assert_eq!(captured.chars().count(), MAX_USER_AGENT_LEN);
        assert!(long.starts_with(&captured));
    }

    #[test]
    fn short_user_agent_is_preserved() {
        let mut headers = HeaderMap::new();
        headers.insert(header::USER_AGENT, HeaderValue::from_static("curl/8.0"));
        assert_eq!(user_agent(&headers), "curl/8.0");
    }

    #[test]
    fn missing_user_agent_is_empty() {
        assert_eq!(user_agent(&HeaderMap::new()), "");
    }
}
