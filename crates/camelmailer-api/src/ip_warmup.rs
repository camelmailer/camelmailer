//! Adaptive IP warm-up over the admin API (global administrators only):
//!
//! * `GET /api/v2/admin/ip_warmup`: every pool address with its mode, the
//!   allowance, today's use and the status per provider group, the last
//!   brake and an estimate of the days until warm, plus recent decisions.
//! * `PATCH /api/v2/admin/ip_warmup/addresses/{id}` and
//!   `PATCH /api/v2/admin/ip_warmup/pools/{id}`: set the mode (`off`,
//!   `slow`, `normal`, `fast`, `custom` with a profile, or `inherit`).
//!   Changing the mode keeps the ramp where it is.
//! * `GET /api/v2/admin/ip_warmup/log`: the decision log.

use crate::app::{
    render_error, render_not_found, render_store_error, render_success, render_validation_error,
    ApiResponse, ApiState, RequestStart,
};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use camelmailer_core::{
    Id, ProviderGroup, WarmupAddress, WarmupLogEntry, WarmupMode, WarmupProfile, WarmupState,
    WarmupStatus, WarmupStore,
};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub(crate) struct SetWarmup {
    /// `off`, `slow`, `normal`, `fast`, `custom` or `inherit`.
    pub mode: String,
    pub profile: Option<WarmupProfile>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct LogParams {
    pub ip_address_id: Option<u64>,
    pub limit: Option<i64>,
}

fn store(state: &ApiState, start: &RequestStart) -> Result<Arc<dyn WarmupStore>, ApiResponse> {
    state.warmup_store.clone().ok_or_else(|| {
        render_error(
            Some(start),
            StatusCode::SERVICE_UNAVAILABLE,
            "WarmupUnavailable",
            "Warm-up needs the PostgreSQL store",
        )
    })
}

fn profile_json(profile: &WarmupProfile) -> Value {
    json!({ "start": profile.start, "growth": profile.growth, "cap": profile.cap })
}

fn log_json(entry: &WarmupLogEntry) -> Value {
    json!({
        "ip_address_id": entry.ip_address_id,
        "provider": entry.provider.map(|p| p.as_str()),
        "decision": entry.decision,
        "reason": entry.reason,
        "allowance_before": entry.allowance_before,
        "allowance_after": entry.allowance_after,
        "created_at": entry.created_at.to_rfc3339(),
    })
}

/// One provider's state as the dashboard shows it.
fn provider_json(
    provider: ProviderGroup,
    state: Option<&WarmupState>,
    profile: Option<&WarmupProfile>,
    used_today: i64,
) -> Value {
    let now = Utc::now();
    let (allowance, status) = match (state, profile) {
        (Some(state), _) => (Some(state.allowance), Some(state.status.as_str())),
        (None, Some(profile)) => (Some(profile.start), Some("not_started")),
        (None, None) => (None, None),
    };
    let warm = state.is_some_and(|s| s.status == WarmupStatus::Warm);
    let eta = match (profile, allowance) {
        (Some(_), _) if warm => Some(0),
        (Some(profile), Some(allowance)) => Some(profile.days_to_warm(allowance)),
        _ => None,
    };
    let progress = match (profile, allowance) {
        (Some(_), _) if warm => Some(1.0),
        (Some(profile), Some(allowance)) => {
            Some(((allowance as f64 / profile.cap as f64) * 1000.0).round() / 1000.0)
        }
        _ => None,
    };
    json!({
        "provider": provider.as_str(),
        "allowance": allowance,
        "used_today": used_today,
        "status": status,
        "paused": state.is_some_and(|s| s.paused_at(now)),
        "paused_until": state.and_then(|s| s.paused_until).map(|t| t.to_rfc3339()),
        "last_reason": state.map(|s| s.last_reason.clone()),
        "last_brake_reason": state.and_then(|s| s.last_brake_reason.clone()),
        "last_brake_at": state.and_then(|s| s.last_brake_at).map(|t| t.to_rfc3339()),
        "evaluated_day": state.and_then(|s| s.evaluated_day).map(|d| d.to_string()),
        "progress": progress,
        "eta_days": eta,
    })
}

fn address_json(
    address: &WarmupAddress,
    states: &[WarmupState],
    usage: &HashMap<(Id, ProviderGroup), i64>,
    log: &[WarmupLogEntry],
) -> Value {
    let (mode, source) = address.effective_mode();
    let profile = address.effective_profile();
    let providers: Vec<Value> = ProviderGroup::ALL
        .iter()
        .map(|provider| {
            let state = states
                .iter()
                .find(|s| s.ip_address_id == address.ip_address_id && s.provider == *provider);
            provider_json(
                *provider,
                state,
                profile.as_ref(),
                usage
                    .get(&(address.ip_address_id, *provider))
                    .copied()
                    .unwrap_or(0),
            )
        })
        .collect();
    let eta = providers
        .iter()
        .filter_map(|p| p["eta_days"].as_u64())
        .max();
    json!({
        "ip_address_id": address.ip_address_id,
        "ipv4": address.ipv4,
        "hostname": address.hostname,
        "pool": { "id": address.ip_pool_id, "name": address.pool_name, "mode": address.pool_mode.map(|m| m.as_str()) },
        "mode": mode.as_str(),
        "mode_source": source,
        "address_mode": address.address_mode.map(|m| m.as_str()),
        // A custom mode without a usable profile counts as off.
        "active": profile.is_some(),
        "profile": profile.as_ref().map(profile_json),
        "providers": providers,
        "eta_days": eta,
        "recent_decisions": log
            .iter()
            .filter(|e| e.ip_address_id == address.ip_address_id)
            .take(10)
            .map(log_json)
            .collect::<Vec<_>>(),
    })
}

pub(crate) async fn ip_warmup_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
) -> ApiResponse {
    let store = match store(&state, &start) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let today = Utc::now().date_naive();
    let loaded = async {
        Ok::<_, camelmailer_core::StoreError>((
            store.warmup_addresses().await?,
            store.warmup_states().await?,
            store.warmup_usage(today).await?,
            store.warmup_log(None, 200).await?,
        ))
    }
    .await;
    let (addresses, states, usage, log) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => return render_store_error(Some(&start), error),
    };
    let usage: HashMap<(Id, ProviderGroup), i64> = usage
        .into_iter()
        .map(|(id, provider, used)| ((id, provider), used))
        .collect();
    let t = &state.config.warmup;
    render_success(
        Some(&start),
        StatusCode::OK,
        json!({
            "profiles": {
                "slow": profile_json(&WarmupProfile::SLOW),
                "normal": profile_json(&WarmupProfile::NORMAL),
                "fast": profile_json(&WarmupProfile::FAST),
            },
            "thresholds": {
                "min_volume_fraction": t.min_volume_fraction,
                "advance_fraction": t.advance_fraction,
                "deferral_rate_slow": t.deferral_rate_slow,
                "bounce_rate_brake": t.bounce_rate_brake,
                "policy_blocks_brake": t.policy_blocks_brake,
                "policy_block_rate_brake": t.policy_block_rate_brake,
                "complaint_rate_warn": t.complaint_rate_warn,
                "complaint_rate_brake": t.complaint_rate_brake,
                "pause_hours": t.pause_hours,
                "step_back": t.step_back,
                "defer_seconds": t.defer_seconds,
            },
            "addresses": addresses
                .iter()
                .map(|a| address_json(a, &states, &usage, &log))
                .collect::<Vec<_>>(),
        }),
    )
}

/// Parse a PATCH body into a mode (`None` = inherit) and profile.
fn parse_body(
    start: &RequestStart,
    body: &SetWarmup,
) -> Result<(Option<WarmupMode>, Option<WarmupProfile>), ApiResponse> {
    if body.mode == "inherit" {
        return Ok((None, None));
    }
    let Some(mode) = WarmupMode::parse(&body.mode) else {
        return Err(render_validation_error(
            Some(start),
            "mode must be off, slow, normal, fast, custom or inherit",
        ));
    };
    match (mode, body.profile) {
        (WarmupMode::Custom, None) => Err(render_validation_error(
            Some(start),
            "mode custom needs a profile {start, growth, cap}",
        )),
        (WarmupMode::Custom, Some(profile)) => match profile.validate() {
            Ok(()) => Ok((Some(mode), Some(profile))),
            Err(message) => Err(render_validation_error(
                Some(start),
                &format!("profile: {message}"),
            )),
        },
        // A profile only matters for custom; drop it otherwise.
        (mode, _) => Ok((Some(mode), None)),
    }
}

fn mode_name(mode: Option<WarmupMode>) -> &'static str {
    mode.map(|m| m.as_str()).unwrap_or("inherit")
}

pub(crate) async fn ip_warmup_address_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
    Json(body): Json<SetWarmup>,
) -> ApiResponse {
    let store = match store(&state, &start) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let (mode, profile) = match parse_body(&start, &body) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let before = match store.warmup_addresses().await {
        Ok(addresses) => addresses.into_iter().find(|a| a.ip_address_id == id),
        Err(error) => return render_store_error(Some(&start), error),
    };
    let Some(before) = before else {
        return render_not_found(Some(&start));
    };
    match store.set_address_warmup(id, mode, profile).await {
        Ok(true) => {}
        Ok(false) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    }
    let entry = WarmupLogEntry {
        ip_address_id: id,
        provider: None,
        decision: "mode".into(),
        reason: format!(
            "mode changed from {} to {}; the ramp continues from its current allowance",
            mode_name(before.address_mode),
            mode_name(mode)
        ),
        allowance_before: None,
        allowance_after: None,
        created_at: Utc::now(),
    };
    if let Err(error) = store.log_warmup(&entry).await {
        tracing::warn!(%error, "could not log the warm-up mode change");
    }
    single_address(&state, &start, &store, id).await
}

pub(crate) async fn ip_warmup_pool_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(id): Path<u64>,
    Json(body): Json<SetWarmup>,
) -> ApiResponse {
    let store = match store(&state, &start) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let (mode, profile) = match parse_body(&start, &body) {
        Ok(parsed) => parsed,
        Err(response) => return response,
    };
    let addresses = match store.warmup_addresses().await {
        Ok(addresses) => addresses,
        Err(error) => return render_store_error(Some(&start), error),
    };
    match store.set_pool_warmup(id, mode, profile).await {
        Ok(true) => {}
        Ok(false) => return render_not_found(Some(&start)),
        Err(error) => return render_store_error(Some(&start), error),
    }
    for address in addresses.iter().filter(|a| a.ip_pool_id == id) {
        let entry = WarmupLogEntry {
            ip_address_id: address.ip_address_id,
            provider: None,
            decision: "mode".into(),
            reason: format!(
                "pool {} default changed from {} to {}",
                address.pool_name,
                mode_name(address.pool_mode),
                mode.map(|m| m.as_str()).unwrap_or("off")
            ),
            allowance_before: None,
            allowance_after: None,
            created_at: Utc::now(),
        };
        if let Err(error) = store.log_warmup(&entry).await {
            tracing::warn!(%error, "could not log the warm-up mode change");
        }
    }
    render_success(
        Some(&start),
        StatusCode::OK,
        json!({ "ip_pool": { "id": id, "mode": mode.map(|m| m.as_str()), "profile": profile.as_ref().map(profile_json) } }),
    )
}

async fn single_address(
    _state: &ApiState,
    start: &RequestStart,
    store: &Arc<dyn WarmupStore>,
    id: Id,
) -> ApiResponse {
    let today = Utc::now().date_naive();
    let loaded = async {
        Ok::<_, camelmailer_core::StoreError>((
            store.warmup_addresses().await?,
            store.warmup_states().await?,
            store.warmup_usage(today).await?,
            store.warmup_log(Some(id), 10).await?,
        ))
    }
    .await;
    match loaded {
        Ok((addresses, states, usage, log)) => {
            let usage: HashMap<(Id, ProviderGroup), i64> = usage
                .into_iter()
                .map(|(id, provider, used)| ((id, provider), used))
                .collect();
            match addresses.iter().find(|a| a.ip_address_id == id) {
                Some(address) => render_success(
                    Some(start),
                    StatusCode::OK,
                    json!({ "address": address_json(address, &states, &usage, &log) }),
                ),
                None => render_not_found(Some(start)),
            }
        }
        Err(error) => render_store_error(Some(start), error),
    }
}

pub(crate) async fn ip_warmup_log(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<LogParams>,
) -> ApiResponse {
    let store = match store(&state, &start) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let limit = params.limit.unwrap_or(100).clamp(1, 500);
    match store.warmup_log(params.ip_address_id, limit).await {
        Ok(entries) => render_success(
            Some(&start),
            StatusCode::OK,
            json!({ "log": entries.iter().map(log_json).collect::<Vec<_>>() }),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}
