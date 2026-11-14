//! Provider feedback for the administration area
//! (`GET /api/v2/admin/deliverability/feedback`): per source whether it is
//! configured, how fresh its data is, the last error, and the newest
//! snapshot per IP or domain. Reserved for administrators of the
//! installation. The data is fetched by the worker
//! (`camelmailer_worker::feedback`); this endpoint only reads it.

use crate::app::{render_store_error, render_success, ApiResponse, ApiState, RequestStart};
use axum::extract::State;
use axum::http::StatusCode;
use camelmailer_core::feedback::source;
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;

/// The provider group each source reports for.
fn provider_of(name: &str) -> &'static str {
    match name {
        source::POSTMASTER_TOOLS => "google",
        source::SNDS => "microsoft",
        _ => "several",
    }
}

pub(crate) async fn feedback_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
) -> ApiResponse {
    let Some(store) = state.feedback_store.as_ref() else {
        return render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "sources": source::ALL.iter().map(|name| json!({
                "source": name, "provider": provider_of(name), "enabled": false,
                "status": null, "freshness_seconds": null, "snapshots": [],
            })).collect::<Vec<Value>>() }),
        );
    };
    let statuses = match store.feedback_source_statuses().await {
        Ok(statuses) => statuses,
        Err(error) => return render_store_error(Some(&start), error),
    };
    let now = Utc::now();
    let mut sources = Vec::new();
    for name in source::ALL {
        let snapshots = match store.latest_feedback_snapshots(name).await {
            Ok(snapshots) => snapshots,
            Err(error) => return render_store_error(Some(&start), error),
        };
        let status = statuses.iter().find(|s| s.source == name);
        // Fetched sources are as fresh as their last successful fetch; the
        // feedback loops as their newest report.
        let fresh_at = match name {
            source::FBL => snapshots.iter().map(|s| s.fetched_at).max(),
            _ => status.and_then(|s| s.last_success_at),
        };
        sources.push(json!({
            "source": name,
            "provider": provider_of(name),
            "enabled": status.is_some_and(|s| s.enabled),
            "status": status,
            "freshness_seconds": fresh_at.map(|at| (now - at).num_seconds().max(0)),
            "snapshots": snapshots,
        }));
    }
    render_success(Some(&start), StatusCode::OK, json!({ "sources": sources }))
}
