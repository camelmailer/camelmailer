//! Organization quota, suspension and monthly usage (admin API).
//!
//! - `GET /api/v2/admin/organizations/{org}/usage?month=YYYY-MM`: the
//!   authoritative monthly counter with limit and remainder. Readable by
//!   every member of the organization (the dashboard's usage view) and by
//!   admin API keys reaching it.
//! - `GET|PATCH …/limits`: `monthly_send_limit`, `suspended`,
//!   `suspension_reason`. Reading follows membership; writing is reserved
//!   for installation administrators (global admins and unscoped admin API
//!   keys), since a quota is the operator's control over a tenant.
//! - `POST …/suspend` (`{ reason }`) and `POST …/unsuspend`: shortcuts with
//!   the same shape as the server endpoints.
//!
//! The send gates live in `server_api::prepare_send` and the SMTP session;
//! [`organization_allowance`] is the HTTP side's view of the policy.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use camelmailer_core::{OrganizationPolicy, SendAllowance, UsageMonth};
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;

use crate::app::{
    find_organization, null_is_explicit, render_error, render_not_found, render_store_error,
    render_success, render_validation_error, ApiResponse, ApiState, Principal, RequestStart,
};

fn policy_json(policy: &OrganizationPolicy) -> Value {
    json!({
        "monthly_send_limit": policy.monthly_send_limit,
        "suspended": policy.suspended,
        "suspension_reason": policy.suspension_reason,
        "retention": policy.retention,
    })
}

/// The installation's retention defaults from configuration.
pub(crate) fn installation_retention(
    config: &camelmailer_config::Config,
) -> camelmailer_core::RetentionPeriods {
    camelmailer_core::RetentionPeriods::from_days(
        config.retention.message_content_days,
        config.message_metadata_retention_days(),
        config.retention.webhook_log_days,
        config.retention.dmarc_report_days,
    )
}

/// The `retention` object of the organization read: effective periods per
/// data class (null = kept forever), the organization's own overrides, and
/// the installation-wide classes.
pub(crate) fn retention_json(state: &ApiState, policy: &OrganizationPolicy) -> Value {
    let effective = installation_retention(&state.config).with_overrides(&policy.retention);
    let days = |value: u32| (value > 0).then_some(value);
    json!({
        "message_content_days": effective.message_content_days,
        "message_metadata_days": effective.message_metadata_days,
        "webhook_log_days": effective.webhook_log_days,
        "dmarc_report_days": effective.dmarc_report_days,
        "auth_event_days": days(state.config.retention.auth_event_days),
        "api_request_days": days(state.config.retention.api_request_days),
        "overrides": policy.retention,
    })
}

/// The organization's allowance for the current month plus its policy, for
/// the HTTP send gate.
pub(crate) async fn organization_allowance(
    state: &ApiState,
    organization_id: camelmailer_core::Id,
) -> Result<(OrganizationPolicy, SendAllowance), camelmailer_core::StoreError> {
    let policy = state.store.organization_policy(organization_id).await?;
    let used = match policy.monthly_send_limit {
        Some(_) => {
            state
                .store
                .organization_send_usage(organization_id, UsageMonth::current())
                .await?
        }
        None => 0,
    };
    let allowance = SendAllowance::organization(policy.monthly_send_limit, used);
    Ok((policy, allowance))
}

#[derive(Debug, Deserialize)]
pub(crate) struct UsageQuery {
    month: Option<String>,
}

/// `GET /api/v2/admin/organizations/{org}/usage?month=YYYY-MM`.
pub(crate) async fn usage_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
    Query(query): Query<UsageQuery>,
) -> ApiResponse {
    let organization = match find_organization(&state, &org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let month = match query.month.as_deref().filter(|m| !m.is_empty()) {
        None => UsageMonth::current(),
        Some(value) => match UsageMonth::parse(value) {
            Some(month) => month,
            None => {
                return render_validation_error(
                    Some(&start.0),
                    "month must be formatted as YYYY-MM",
                )
            }
        },
    };
    let policy = match state.store.organization_policy(organization.id).await {
        Ok(policy) => policy,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let sent = match state
        .store
        .organization_send_usage(organization.id, month)
        .await
    {
        Ok(sent) => sent,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    // The limit is the current one: limits are not versioned per month, so
    // a past month reports usage against today's limit.
    let allowance = SendAllowance::organization(policy.monthly_send_limit, sent);
    let period_start = month.first_day().and_hms_opt(0, 0, 0).expect("midnight");
    let period_end = month
        .next_first_day()
        .and_hms_opt(0, 0, 0)
        .expect("midnight");
    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "usage": {
                "organization": organization.permalink,
                "month": month.label(),
                "period_start": period_start.and_utc(),
                "period_end": period_end.and_utc(),
                "sent": sent,
                "limit": policy.monthly_send_limit,
                "remaining": allowance.remaining(),
                "suspended": policy.suspended,
                "suspension_reason": policy.suspension_reason,
            }
        }),
    )
}

/// `GET /api/v2/admin/organizations/{org}/limits`.
pub(crate) async fn limits_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    let organization = match find_organization(&state, &org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    match state.store.organization_policy(organization.id).await {
        Ok(policy) => render_success(
            Some(&start.0),
            StatusCode::OK,
            json!({ "limits": policy_json(&policy) }),
        ),
        Err(error) => render_store_error(Some(&start.0), error),
    }
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct UpdateLimits {
    /// Double option: absent leaves the limit alone, `null` clears it.
    #[serde(default, deserialize_with = "null_is_explicit")]
    monthly_send_limit: Option<Option<i64>>,
    suspended: Option<bool>,
    #[serde(default, deserialize_with = "null_is_explicit")]
    suspension_reason: Option<Option<String>>,
    /// Per-class retention overrides; each field is a double option too
    /// (absent: unchanged, null: inherit the installation default, 0: keep
    /// forever, n: keep n days).
    #[serde(default)]
    retention: Option<UpdateRetention>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateRetention {
    #[serde(default, deserialize_with = "null_is_explicit")]
    message_content_days: Option<Option<i64>>,
    #[serde(default, deserialize_with = "null_is_explicit")]
    message_metadata_days: Option<Option<i64>>,
    #[serde(default, deserialize_with = "null_is_explicit")]
    webhook_log_days: Option<Option<i64>>,
    #[serde(default, deserialize_with = "null_is_explicit")]
    dmarc_report_days: Option<Option<i64>>,
}

fn apply_days(target: &mut Option<u32>, change: Option<Option<i64>>) -> Result<(), &'static str> {
    if let Some(value) = change {
        *target = match value {
            None => None,
            Some(days) if (0..=36_500).contains(&days) => Some(days as u32),
            Some(_) => {
                return Err("Retention periods must be between 0 (keep forever) and 36500 days, or null to inherit")
            }
        };
    }
    Ok(())
}

fn require_root(principal: &Principal, start: &RequestStart) -> Option<ApiResponse> {
    if principal.is_root() {
        return None;
    }
    Some(render_error(
        Some(start),
        StatusCode::FORBIDDEN,
        "AccessDenied",
        "Only an installation administrator can change an organization's limits",
    ))
}

async fn apply(
    state: &ApiState,
    start: &RequestStart,
    org_permalink: &str,
    change: impl FnOnce(&mut OrganizationPolicy) -> Result<(), &'static str>,
) -> ApiResponse {
    let organization = match find_organization(state, org_permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(start)),
        Err(error) => return render_store_error(Some(start), error),
    };
    let mut policy = match state.store.organization_policy(organization.id).await {
        Ok(policy) => policy,
        Err(error) => return render_store_error(Some(start), error),
    };
    if let Err(message) = change(&mut policy) {
        return render_validation_error(Some(start), message);
    }
    if let Err(error) = state
        .store
        .set_organization_policy(organization.id, &policy)
        .await
    {
        return render_store_error(Some(start), error);
    }
    render_success(
        Some(start),
        StatusCode::OK,
        json!({ "limits": policy_json(&policy) }),
    )
}

/// `PATCH /api/v2/admin/organizations/{org}/limits` (installation admins).
pub(crate) async fn limits_update(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path(org_permalink): Path<String>,
    Json(body): Json<UpdateLimits>,
) -> ApiResponse {
    if let Some(denied) = require_root(&principal.0, &start.0) {
        return denied;
    }
    apply(&state, &start.0, &org_permalink, |policy| {
        if let Some(limit) = body.monthly_send_limit {
            if limit.is_some_and(|limit| limit < 0) {
                return Err("Monthly send limit must be zero or greater, or null for unlimited");
            }
            policy.monthly_send_limit = limit;
        }
        if let Some(suspended) = body.suspended {
            policy.suspended = suspended;
            if !suspended {
                policy.suspension_reason = None;
            }
        }
        if let Some(reason) = body.suspension_reason {
            policy.suspension_reason = reason;
        }
        if let Some(retention) = body.retention {
            apply_days(
                &mut policy.retention.message_content_days,
                retention.message_content_days,
            )?;
            apply_days(
                &mut policy.retention.message_metadata_days,
                retention.message_metadata_days,
            )?;
            apply_days(
                &mut policy.retention.webhook_log_days,
                retention.webhook_log_days,
            )?;
            apply_days(
                &mut policy.retention.dmarc_report_days,
                retention.dmarc_report_days,
            )?;
        }
        if policy.suspended && policy.suspension_reason.is_none() {
            policy.suspension_reason = Some("Suspended via Admin API".into());
        }
        Ok(())
    })
    .await
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct SuspendBody {
    reason: Option<String>,
}

/// `POST /api/v2/admin/organizations/{org}/suspend` (installation admins).
pub(crate) async fn organization_suspend(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path(org_permalink): Path<String>,
    body: Option<Json<SuspendBody>>,
) -> ApiResponse {
    if let Some(denied) = require_root(&principal.0, &start.0) {
        return denied;
    }
    let reason = body
        .and_then(|Json(body)| body.reason)
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| "Suspended via Admin API".into());
    apply(&state, &start.0, &org_permalink, |policy| {
        policy.suspended = true;
        policy.suspension_reason = Some(reason);
        Ok(())
    })
    .await
}

/// `POST /api/v2/admin/organizations/{org}/unsuspend` (installation admins).
pub(crate) async fn organization_unsuspend(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    principal: axum::Extension<Principal>,
    Path(org_permalink): Path<String>,
) -> ApiResponse {
    if let Some(denied) = require_root(&principal.0, &start.0) {
        return denied;
    }
    apply(&state, &start.0, &org_permalink, |policy| {
        policy.suspended = false;
        policy.suspension_reason = None;
        Ok(())
    })
    .await
}
