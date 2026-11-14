//! Provider feedback for the administration area
//! (`GET /api/v2/admin/deliverability/feedback`): per source whether it is
//! configured, how fresh its data is, the last error, and the newest
//! snapshot per IP or domain. Reserved for administrators of the
//! installation. The data is fetched by the worker
//! (`camelmailer_worker::feedback`); this endpoint only reads it.

use crate::app::{
    find_server, render_error, render_not_found, render_store_error, render_success,
    render_validation_error, ApiResponse, ApiState, RequestStart,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
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
            source::FBL | source::PLACEMENT => snapshots.iter().map(|s| s.fetched_at).max(),
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

/// Whether this installation's config has seed placement tests.
fn placement_configured(config: &camelmailer_config::Config) -> bool {
    let placement = &config.deliverability_feedback.placement;
    !placement.seeds.is_empty()
        && !placement.server.trim().is_empty()
        && placement.from_address.contains('@')
}

/// `GET /api/v2/admin/deliverability/placement_tests`: the placement
/// history, newest first (200 by default, `limit` up to 1000), optionally
/// for one `ip` and `provider` group, with the last round.
/// Filters of the placement history.
#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct PlacementQuery {
    ip: Option<String>,
    provider: Option<String>,
    limit: Option<i64>,
}

pub(crate) async fn placement_tests_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    axum::extract::Query(query): axum::extract::Query<PlacementQuery>,
) -> ApiResponse {
    let configured = placement_configured(&state.config);
    let Some(store) = state.feedback_store.as_ref() else {
        return render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "configured": configured, "last_round": null, "placement_tests": [] }),
        );
    };
    let last_round = match store.last_placement_round().await {
        Ok(round) => round,
        Err(error) => return render_store_error(Some(&start), error),
    };
    let filter = camelmailer_core::PlacementFilter {
        ip: query.ip.filter(|ip| !ip.is_empty()),
        provider: query.provider.filter(|p| !p.is_empty()),
        limit: query.limit.unwrap_or(200).clamp(1, 1000),
    };
    match store.list_placement_tests(&filter).await {
        Ok(tests) => render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "configured": configured, "last_round": last_round, "placement_tests": tests }),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}

/// `POST /api/v2/admin/deliverability/placement_tests`: request a round now.
/// A worker sends it within a minute. At most one round per
/// `deliverability_feedback.placement.min_round_interval_seconds`.
pub(crate) async fn placement_tests_create(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
) -> ApiResponse {
    if !placement_configured(&state.config) {
        return render_error(
            Some(&start),
            StatusCode::CONFLICT,
            "PlacementNotConfigured",
            "Seed placement tests need deliverability_feedback.placement.server, from_address and at least one seed",
        );
    }
    let Some(store) = state.feedback_store.as_ref() else {
        return render_error(
            Some(&start),
            StatusCode::CONFLICT,
            "PlacementNotConfigured",
            "This installation keeps no provider feedback",
        );
    };
    let min_interval = i64::from(
        state
            .config
            .deliverability_feedback
            .placement
            .min_round_interval_seconds,
    );
    match store.request_placement_round("manual", min_interval).await {
        Ok(Some(round)) => render_success(
            Some(&start),
            StatusCode::ACCEPTED,
            json!({ "placement_round": round }),
        ),
        Ok(None) => render_error(
            Some(&start),
            StatusCode::CONFLICT,
            "PlacementRateLimited",
            &format!("A placement round was requested less than {min_interval} seconds ago"),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}

/// The complaint setting of one server as the API shows it.
fn complaint_settings_json(own: Option<bool>, default: bool) -> Value {
    json!({
        "suppress_complainants": own,
        "installation_default": default,
        "effective": own.unwrap_or(default),
    })
}

/// `GET /api/v2/admin/organizations/{org}/servers/{server}/complaint_settings`:
/// whether a feedback-loop complaint about this server's transactional mail
/// suppresses the recipient. `suppress_complainants` is the server's own
/// choice (`null` follows the installation default).
pub(crate) async fn complaint_settings_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
) -> ApiResponse {
    let server = match find_server(&state, &org, &server).await {
        Ok(Some(server)) => server,
        Ok(None) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    };
    let default = state
        .config
        .deliverability_feedback
        .fbl
        .suppress_complainants;
    let own = match state.feedback_store.as_ref() {
        Some(store) => match store.server_complaint_suppression(server.id).await {
            Ok(own) => own,
            Err(error) => return render_store_error(Some(&start), error),
        },
        None => None,
    };
    render_success(
        Some(&start),
        StatusCode::OK,
        json!({ "complaint_settings": complaint_settings_json(own, default) }),
    )
}

/// `PATCH …/complaint_settings` with `{"suppress_complainants": true | false | null}`.
pub(crate) async fn complaint_settings_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path((org, server)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResponse {
    let own = match body.get("suppress_complainants") {
        Some(Value::Bool(value)) => Some(*value),
        Some(Value::Null) => None,
        _ => {
            return render_validation_error(
                Some(&start),
                "suppress_complainants must be true, false or null",
            )
        }
    };
    let server = match find_server(&state, &org, &server).await {
        Ok(Some(server)) => server,
        Ok(None) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    };
    let Some(store) = state.feedback_store.as_ref() else {
        return render_validation_error(
            Some(&start),
            "This installation keeps no complaint settings",
        );
    };
    if let Err(error) = store.set_server_complaint_suppression(server.id, own).await {
        return render_store_error(Some(&start), error);
    }
    let default = state
        .config
        .deliverability_feedback
        .fbl
        .suppress_complainants;
    render_success(
        Some(&start),
        StatusCode::OK,
        json!({ "complaint_settings": complaint_settings_json(own, default) }),
    )
}
