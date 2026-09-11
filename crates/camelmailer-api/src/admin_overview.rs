//! Instance-wide oversight for the administration area
//! (`GET /api/v2/admin/overview` and
//! `GET /api/v2/admin/organizations/{permalink}/overview`).
//!
//! Both endpoints answer the same question at two zoom levels: how much
//! mail is moving through this installation, and which tenant it belongs
//! to. The instance view rolls every organization up so unusual traffic
//! stands out early; the organization view breaks one tenant down by
//! server. Judgment stays in the dashboard: these endpoints report
//! counters, never a verdict.
//!
//! Cost: one query per organization for its servers, one for its members,
//! and one per server for its traffic (three nested windows in a single
//! aggregate, see [`camelmailer_core::ServerStore::message_volume`]).
//! Both endpoints are reserved for administrators of the installation and
//! are not on any hot path.

use crate::app::{
    find_organization, render_error, render_not_found, render_store_error, render_success,
    ApiResponse, ApiState, RequestStart,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use camelmailer_core::{
    MessageVolume, Organization, Server, ServerMode, StoreError, VolumeCounters, VolumeWindows,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::sync::Arc;

/// The three windows every rollup reports, as offsets from "now".
fn windows_from(now: DateTime<Utc>) -> VolumeWindows {
    VolumeWindows {
        day: now - Duration::hours(24),
        week: now - Duration::days(7),
        month: now - Duration::days(30),
    }
}

fn counters_json(counters: &VolumeCounters) -> Value {
    json!({
        "total": counters.total,
        "outgoing": counters.outgoing,
        "incoming": counters.incoming,
        "sent": counters.sent,
        "held": counters.held,
        "failed": counters.failed,
        "bounced": counters.bounced,
    })
}

fn add_counters(total: &mut VolumeCounters, part: &VolumeCounters) {
    total.total += part.total;
    total.outgoing += part.outgoing;
    total.incoming += part.incoming;
    total.sent += part.sent;
    total.held += part.held;
    total.failed += part.failed;
    total.bounced += part.bounced;
}

/// Sum one server's volume into a running total, keeping the widest span
/// of activity.
fn add_volume(total: &mut MessageVolume, part: &MessageVolume) {
    add_counters(&mut total.day, &part.day);
    add_counters(&mut total.week, &part.week);
    add_counters(&mut total.month, &part.month);
    total.first_message_at = match (total.first_message_at, part.first_message_at) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    total.last_message_at = match (total.last_message_at, part.last_message_at) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
}

/// The window counters and activity span, flattened into the object a
/// rollup row already carries.
fn volume_fields(volume: &MessageVolume) -> Vec<(String, Value)> {
    vec![
        ("day".to_string(), counters_json(&volume.day)),
        ("week".to_string(), counters_json(&volume.week)),
        ("month".to_string(), counters_json(&volume.month)),
        (
            "first_message_at".to_string(),
            json!(volume.first_message_at),
        ),
        ("last_message_at".to_string(), json!(volume.last_message_at)),
    ]
}

fn with_volume(mut object: Value, volume: &MessageVolume) -> Value {
    if let Some(map) = object.as_object_mut() {
        for (key, value) in volume_fields(volume) {
            map.insert(key, value);
        }
    }
    object
}

/// One organization's traffic, summed over its servers, alongside the
/// per-server breakdown the organization view renders.
struct OrganizationRollup {
    organization: Organization,
    servers: Vec<(Server, MessageVolume)>,
    members: usize,
    volume: MessageVolume,
}

impl OrganizationRollup {
    fn suspended_servers(&self) -> usize {
        self.servers
            .iter()
            .filter(|(server, _)| server.suspended)
            .count()
    }

    /// The row the instance view lists, without the per-server detail.
    fn summary_json(&self) -> Value {
        with_volume(
            json!({
                "name": self.organization.name,
                "permalink": self.organization.permalink,
                "require_two_factor": self.organization.require_two_factor,
                "servers": self.servers.len(),
                "suspended_servers": self.suspended_servers(),
                "members": self.members,
            }),
            &self.volume,
        )
    }
}

/// Gather one organization's rollup: its servers with their traffic, its
/// member count, and the sum across the organization.
async fn rollup(
    state: &ApiState,
    server_store: &dyn camelmailer_core::ServerStore,
    organization: Organization,
    windows: VolumeWindows,
) -> Result<OrganizationRollup, StoreError> {
    let servers = state
        .store
        .servers_for_organization(organization.id)
        .await?;
    let mut with_volumes = Vec::with_capacity(servers.len());
    let mut total = MessageVolume::default();
    for server in servers {
        let volume = server_store.message_volume(server.id, windows).await?;
        add_volume(&mut total, &volume);
        with_volumes.push((server, volume));
    }
    // Accounts storage is optional; without it an organization has no
    // members to count rather than an error.
    let members = match state.auth_store.as_ref() {
        Some(auth_store) => auth_store
            .memberships_for_organization(organization.id)
            .await?
            .len(),
        None => 0,
    };
    Ok(OrganizationRollup {
        organization,
        servers: with_volumes,
        members,
        volume: total,
    })
}

/// `GET /api/v2/admin/overview` — every organization on the installation
/// with its traffic over the last 24 hours, 7 days and 30 days, plus the
/// instance totals.
///
/// Reserved for administrators: the central `auth_middleware` answers 403
/// for a user session without the global admin flag and 404 for a scoped
/// machine key, the same treatment every other global resource gets.
/// Organizations come back busiest first (30-day volume, then name), so
/// the tenant worth looking at leads the list.
pub(crate) async fn overview_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
) -> ApiResponse {
    let Some(server_store) = state.server_store.as_ref() else {
        return storage_unavailable(Some(&start.0));
    };
    let now = Utc::now();
    let windows = windows_from(now);

    let organizations = match state.store.list_organizations().await {
        Ok(organizations) => organizations,
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let mut rollups = Vec::with_capacity(organizations.len());
    for organization in organizations {
        match rollup(&state, server_store.as_ref(), organization, windows).await {
            Ok(rollup) => rollups.push(rollup),
            Err(error) => return render_store_error(Some(&start.0), error),
        }
    }
    rollups.sort_by(|a, b| {
        b.volume
            .month
            .total
            .cmp(&a.volume.month.total)
            .then_with(|| a.organization.name.cmp(&b.organization.name))
    });

    let mut instance = MessageVolume::default();
    for rollup in &rollups {
        add_volume(&mut instance, &rollup.volume);
    }
    let users = match state.store.list_users().await {
        Ok(users) => users.len(),
        Err(error) => return render_store_error(Some(&start.0), error),
    };

    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "overview": {
                "generated_at": now,
                "windows": {
                    "day": windows.day,
                    "week": windows.week,
                    "month": windows.month,
                },
                "instance": with_volume(
                    json!({
                        "organizations": rollups.len(),
                        "servers": rollups.iter().map(|r| r.servers.len()).sum::<usize>(),
                        "suspended_servers": rollups
                            .iter()
                            .map(OrganizationRollup::suspended_servers)
                            .sum::<usize>(),
                        "users": users,
                    }),
                    &instance,
                ),
                "organizations": rollups
                    .iter()
                    .map(OrganizationRollup::summary_json)
                    .collect::<Vec<_>>(),
            }
        }),
    )
}

/// `GET /api/v2/admin/organizations/{permalink}/overview` — one
/// organization's traffic with its servers broken out.
///
/// A read any member of the organization may perform (the central RBAC
/// treats it like the other organization reads, and a non-member gets 404
/// so the organization's existence stays private).
pub(crate) async fn organization_overview_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Path(permalink): Path<String>,
) -> ApiResponse {
    let organization = match find_organization(&state, &permalink).await {
        Ok(Some(organization)) => organization,
        Ok(None) => return render_not_found(Some(&start.0)),
        Err(error) => return render_store_error(Some(&start.0), error),
    };
    let Some(server_store) = state.server_store.as_ref() else {
        return storage_unavailable(Some(&start.0));
    };
    let now = Utc::now();
    let windows = windows_from(now);
    let rollup = match rollup(&state, server_store.as_ref(), organization, windows).await {
        Ok(rollup) => rollup,
        Err(error) => return render_store_error(Some(&start.0), error),
    };

    render_success(
        Some(&start.0),
        StatusCode::OK,
        json!({
            "overview": {
                "generated_at": now,
                "windows": {
                    "day": windows.day,
                    "week": windows.week,
                    "month": windows.month,
                },
                "organization": rollup.summary_json(),
                "servers": rollup
                    .servers
                    .iter()
                    .map(|(server, volume)| with_volume(
                        json!({
                            "name": server.name,
                            "permalink": server.permalink,
                            "mode": match server.mode {
                                ServerMode::Live => "Live",
                                ServerMode::Development => "Development",
                            },
                            "suspended": server.suspended,
                            "suspension_reason": server.suspension_reason,
                            "send_limit": server.send_limit,
                        }),
                        volume,
                    ))
                    .collect::<Vec<_>>(),
            }
        }),
    )
}

/// Reported when statistics storage is absent, so a caller can tell
/// "nothing sent" from "counters unavailable". Zeros would read as real
/// numbers; the wording matches the per-server statistics endpoints.
fn storage_unavailable(start: Option<&RequestStart>) -> ApiResponse {
    render_error(
        start,
        StatusCode::SERVICE_UNAVAILABLE,
        "StorageUnavailable",
        "Per-server statistics storage is not configured",
    )
}
