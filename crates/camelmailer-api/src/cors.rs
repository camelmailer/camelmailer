//! CORS for browser frontends, driven by `web_server.cors_origins`.
//!
//! Empty list (the default): no CORS headers at all — the APIs stay
//! same-origin/back-end only. `["*"]` allows any origin (fine with Bearer
//! tokens; the APIs use no cookies, so no credentialed CORS is involved).
//! Otherwise the listed origins are allowed exactly.

use axum::http::{HeaderName, HeaderValue, Method};
use tower_http::cors::{AllowOrigin, CorsLayer};

/// Build the CORS layer for the configured origins; `None` when CORS is
/// not enabled.
pub fn cors_layer(origins: &[String]) -> Option<CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let allow_origin = if origins.iter().any(|origin| origin == "*") {
        AllowOrigin::any()
    } else {
        AllowOrigin::list(
            origins
                .iter()
                .filter_map(|origin| origin.parse::<HeaderValue>().ok()),
        )
    };
    Some(
        CorsLayer::new()
            .allow_origin(allow_origin)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PATCH,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([
                HeaderName::from_static("authorization"),
                HeaderName::from_static("content-type"),
                HeaderName::from_static("x-admin-api-key"),
                HeaderName::from_static("x-server-api-key"),
            ])
            .max_age(std::time::Duration::from_secs(3600)),
    )
}
