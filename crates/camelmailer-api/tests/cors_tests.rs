//! CORS behaviour of the composed API router.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camelmailer_api::{build_auth_router, cors_layer, ApiState};
use camelmailer_core::MemoryStore;
use std::sync::Arc;
use tower::ServiceExt;

fn app(origins: &[&str]) -> Router {
    let store = Arc::new(MemoryStore::new());
    let mut config = camelmailer_config::Config::default();
    config.web_server.cors_origins = origins.iter().map(|s| s.to_string()).collect();
    let state = ApiState::full(store.clone(), None, Some(store), None, config.clone());
    let mut router = build_auth_router(state);
    if let Some(cors) = cors_layer(&config.web_server.cors_origins) {
        router = router.layer(cors);
    }
    router
}

async fn preflight(app: &Router, origin: &str) -> (StatusCode, Option<String>, Option<String>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("OPTIONS")
                .uri("/api/v2/auth/login")
                .header("origin", origin)
                .header("access-control-request-method", "POST")
                .header(
                    "access-control-request-headers",
                    "authorization,content-type",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    (
        response.status(),
        header("access-control-allow-origin"),
        header("access-control-allow-headers"),
    )
}

#[tokio::test]
async fn no_cors_headers_without_configuration() {
    let app = app(&[]);
    let (_, allow_origin, _) = preflight(&app, "https://app.example.com").await;
    assert_eq!(allow_origin, None);
}

#[tokio::test]
async fn listed_origins_pass_preflight_and_carry_headers() {
    let app = app(&["https://app.example.com"]);

    let (status, allow_origin, allow_headers) = preflight(&app, "https://app.example.com").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(allow_origin.as_deref(), Some("https://app.example.com"));
    let allow_headers = allow_headers.unwrap().to_lowercase();
    for header in [
        "authorization",
        "content-type",
        "x-admin-api-key",
        "x-server-api-key",
    ] {
        assert!(allow_headers.contains(header), "missing {header}");
    }

    // an unlisted origin gets no allow-origin header
    let (_, allow_origin, _) = preflight(&app, "https://evil.example.com").await;
    assert_eq!(allow_origin, None);

    // actual (non-preflight) responses carry the header too
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v2/auth/login")
                .header("origin", "https://app.example.com")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"email_address":"a@b.c","password":"x"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .and_then(|value| value.to_str().ok()),
        Some("https://app.example.com")
    );
}

#[tokio::test]
async fn wildcard_allows_any_origin() {
    let app = app(&["*"]);
    let (status, allow_origin, _) = preflight(&app, "https://anywhere.example").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(allow_origin.as_deref(), Some("*"));
}
