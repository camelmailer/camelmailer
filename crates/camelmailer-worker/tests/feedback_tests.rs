//! The provider feedback fetcher end to end, against local mocks of the
//! Google OAuth token endpoint, the Postmaster Tools API v2 and the SNDS
//! automated data access URL. No network.

use std::sync::{Arc, Mutex};

use camelmailer_core::feedback::{provider, source, Verdict};
use camelmailer_core::{FeedbackStore, MemoryStore};
use camelmailer_worker::feedback::FeedbackFetcher;
use tokio::net::TcpListener;

#[derive(Default)]
struct Seen {
    token_forms: Vec<String>,
    queries: Vec<(String, Option<String>, serde_json::Value)>,
}

async fn mock_provider(snds_status: u16) -> (String, Arc<Mutex<Seen>>) {
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = axum::Router::new()
        .route(
            "/token",
            axum::routing::post(
                |State(seen): State<Arc<Mutex<Seen>>>, body: String| async move {
                    seen.lock().unwrap().token_forms.push(body);
                    axum::Json(serde_json::json!({
                        "access_token": "ya29.test",
                        "expires_in": 3599,
                        "token_type": "Bearer"
                    }))
                },
            ),
        )
        .route(
            "/v2/domains/{*rest}",
            axum::routing::post(
                |State(seen): State<Arc<Mutex<Seen>>>,
                 Path(rest): Path<String>,
                 headers: HeaderMap,
                 axum::Json(body): axum::Json<serde_json::Value>| async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    seen.lock().unwrap().queries.push((rest, auth, body));
                    (
                        [("content-type", "application/json")],
                        include_str!("fixtures/postmaster_domain_stats.json"),
                    )
                },
            ),
        )
        .route(
            "/snds",
            axum::routing::get(move || async move {
                match snds_status {
                    200 => (
                        StatusCode::OK,
                        include_str!("fixtures/snds_data.csv").to_string(),
                    ),
                    308 => (StatusCode::PERMANENT_REDIRECT, String::new()),
                    other => (StatusCode::from_u16(other).unwrap(), String::new()),
                }
            }),
        )
        .with_state(seen.clone());
    tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });
    (base, seen)
}

fn config(base: &str) -> camelmailer_config::Config {
    let mut config = camelmailer_config::Config::default();
    let feedback = &mut config.deliverability_feedback;
    feedback.postmaster_tools.domains = vec!["Example.com".into()];
    feedback.postmaster_tools.client_id = Some("client-id".into());
    feedback.postmaster_tools.api_base_url = base.to_string();
    feedback.postmaster_tools.token_url = format!("{base}/token");
    config
}

fn secrets(base: String) -> impl Fn(&str) -> Option<String> + Send + Sync + 'static {
    move |name: &str| match name {
        "CAMELMAILER_POSTMASTER_CLIENT_SECRET" => Some("client-secret".into()),
        "CAMELMAILER_POSTMASTER_REFRESH_TOKEN" => Some("refresh-token".into()),
        "CAMELMAILER_SNDS_DATA_URL" => Some(format!("{base}/snds?key=k")),
        _ => None,
    }
}

#[tokio::test]
async fn both_sources_are_fetched_once_and_stored_with_verdicts() {
    let (base, seen) = mock_provider(200).await;
    let store = Arc::new(MemoryStore::new());
    let fetcher =
        FeedbackFetcher::new(&config(&base), store.clone()).with_lookup(secrets(base.clone()));

    let done = fetcher.run_once().await;
    assert_eq!(done.len(), 2, "{done:?}");
    assert!(done.iter().all(|(_, r)| r.is_ok()), "{done:?}");

    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.token_forms.len(), 1);
        assert!(seen.token_forms[0].contains("grant_type=refresh_token"));
        assert!(seen.token_forms[0].contains("refresh_token=refresh-token"));
        let (path, auth, body) = &seen.queries[0];
        assert_eq!(path, "example.com/domainStats:query");
        assert_eq!(auth.as_deref(), Some("Bearer ya29.test"));
        assert_eq!(body["aggregationGranularity"], "DAILY");
    }

    let google = store
        .latest_feedback_snapshots(source::POSTMASTER_TOOLS)
        .await
        .unwrap();
    assert_eq!(google.len(), 1, "newest day per domain");
    assert_eq!(google[0].subject, "example.com");
    assert_eq!(google[0].verdict, Verdict::Bad);

    let snds = store.latest_feedback_snapshots(source::SNDS).await.unwrap();
    assert_eq!(snds.len(), 3);
    assert_eq!(snds[0].subject, "198.51.100.7");

    let signals = store
        .latest_provider_signals("192.0.2.11", provider::MICROSOFT)
        .await
        .unwrap();
    assert_eq!(signals.len(), 1);
    assert_eq!(signals[0].verdict, Verdict::Warn);

    // Not due again until the interval has passed: a second round fetches
    // nothing.
    assert!(fetcher.run_once().await.is_empty());
    let statuses = store.feedback_source_statuses().await.unwrap();
    assert_eq!(statuses.len(), 3);
    assert!(statuses.iter().all(|s| s.enabled));
    assert!(statuses
        .iter()
        .filter(|s| s.source != source::FBL)
        .all(|s| s.last_success_at.is_some() && s.last_error.is_none()));
}

#[tokio::test]
async fn a_redirecting_snds_url_is_reported_without_its_key() {
    let (base, _) = mock_provider(308).await;
    let store = Arc::new(MemoryStore::new());
    let mut config = config(&base);
    config
        .deliverability_feedback
        .postmaster_tools
        .domains
        .clear();
    let fetcher = FeedbackFetcher::new(&config, store.clone()).with_lookup(secrets(base));
    let done = fetcher.run_once().await;
    assert_eq!(done.len(), 1);
    let error = done[0].1.clone().unwrap_err();
    assert!(error.contains("redirects"), "{error}");
    let status = store
        .feedback_source_statuses()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.source == source::SNDS)
        .unwrap();
    assert_eq!(status.consecutive_failures, 1);
    assert!(!status.last_error.unwrap().contains("key=k"));
    let postmaster = store
        .feedback_source_statuses()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.source == source::POSTMASTER_TOOLS)
        .unwrap();
    assert!(!postmaster.enabled, "no domains: Postmaster Tools is off");
}

#[tokio::test]
async fn without_credentials_nothing_is_fetched_and_nothing_fails() {
    let store = Arc::new(MemoryStore::new());
    let fetcher = FeedbackFetcher::new(&camelmailer_config::Config::default(), store.clone())
        .with_lookup(|_| None);
    assert!(fetcher.run_once().await.is_empty());
    let statuses = store.feedback_source_statuses().await.unwrap();
    let enabled: Vec<(&str, bool)> = statuses
        .iter()
        .map(|s| (s.source.as_str(), s.enabled))
        .collect();
    assert_eq!(
        enabled,
        vec![("postmaster_tools", false), ("snds", false), ("fbl", true)]
    );
}
