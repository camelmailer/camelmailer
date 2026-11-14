//! The cluster topology for the infrastructure dashboard
//! (`GET /api/v2/admin/cluster`): which processes run where, in which role
//! and version, when each was last heard from, which worker holds which
//! queue leases, and the addresses the installation sends and receives on.
//!
//! The processes write their own entries (`camelmailer::cluster`); this
//! endpoint only reads. Reserved for administrators of the installation.

use crate::app::{render_store_error, render_success, ApiResponse, ApiState, RequestStart};
use axum::extract::State;
use axum::http::StatusCode;
use camelmailer_core::{ClusterNode, NodeLeases};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;

/// How a registered process is doing, from its heartbeat.
pub(crate) fn node_status(
    node: &ClusterNode,
    now: DateTime<Utc>,
    stale_seconds: u32,
) -> &'static str {
    if node.stopped_at.is_some() {
        "stopped"
    } else if (now - node.last_seen_at).num_seconds() <= i64::from(stale_seconds) {
        "live"
    } else {
        "stale"
    }
}

fn leases_json(leases: Option<&NodeLeases>, now: DateTime<Utc>) -> Value {
    let leases = leases.cloned().unwrap_or_default();
    json!({
        "queued_messages": leases.queued_messages,
        "webhook_requests": leases.webhook_requests,
        "lifecycle_events": leases.lifecycle_events,
        "oldest_lock_at": leases.oldest_lock_at,
        "oldest_lock_age_seconds": leases.oldest_lock_at.map(|at| (now - at).num_seconds().max(0)),
    })
}

/// The topology document, built from what the store returned.
pub(crate) fn topology_json(
    config: &camelmailer_config::Config,
    nodes: &[ClusterNode],
    leases: &[NodeLeases],
    pools: Vec<Value>,
    now: DateTime<Utc>,
) -> Value {
    let stale = config.cluster.node_stale_seconds;
    let mut live_versions = BTreeSet::new();
    let nodes_json: Vec<Value> = nodes
        .iter()
        .map(|node| {
            let status = node_status(node, now, stale);
            if status == "live" {
                live_versions.insert(node.version.clone());
            }
            let held = leases.iter().find(|l| l.locked_by == node.instance_id);
            json!({
                "instance_id": node.instance_id,
                "role": node.role,
                "node_name": node.node_name,
                "version": node.version,
                "status": status,
                "started_at": node.started_at,
                "last_seen_at": node.last_seen_at,
                "seconds_since_seen": (now - node.last_seen_at).num_seconds().max(0),
                "stopped_at": node.stopped_at,
                "details": node.details,
                "leases": if node.role == "worker" { leases_json(held, now) } else { Value::Null },
            })
        })
        .collect();
    // Locks held under an id no registered worker has: a worker from before
    // registration existed, or one whose entry was pruned. They are taken
    // over once their lease runs out.
    let unregistered: Vec<Value> = leases
        .iter()
        .filter(|l| !nodes.iter().any(|n| n.instance_id == l.locked_by))
        .map(|l| {
            let mut value = leases_json(Some(l), now);
            value["locked_by"] = json!(l.locked_by);
            value
        })
        .collect();
    json!({
        "cluster": {
            "load_balancer_address": config.cluster.load_balancer(),
            "inbound_hostname": config.cluster.inbound_host(),
            "heartbeat_interval_seconds": config.cluster.heartbeat_interval_seconds,
            "node_stale_seconds": stale,
        },
        "nodes": nodes_json,
        "unregistered_leases": unregistered,
        "versions": live_versions.iter().collect::<Vec<_>>(),
        "mixed_versions": live_versions.len() > 1,
        "ip_pools": pools,
    })
}

/// `GET /api/v2/admin/cluster`.
pub(crate) async fn cluster_show(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
) -> ApiResponse {
    let store = &state.store;
    let result = async {
        let nodes = store.list_cluster_nodes().await?;
        let leases = store.cluster_leases().await?;
        let mut pools = Vec::new();
        for pool in store.list_ip_pools().await? {
            let addresses = store.list_ip_addresses(pool.id).await?;
            pools.push(json!({
                "id": pool.id,
                "name": pool.name,
                "default": pool.default,
                "address_family": pool.address_family,
                "addresses": addresses.iter().map(|a| json!({
                    "id": a.id,
                    "ipv4": a.ipv4,
                    "ipv6": a.ipv6,
                    "hostname": a.hostname,
                    "priority": a.priority,
                })).collect::<Vec<_>>(),
            }));
        }
        Ok::<_, camelmailer_core::StoreError>((nodes, leases, pools))
    }
    .await;
    match result {
        Ok((nodes, leases, pools)) => render_success(
            Some(&start),
            StatusCode::OK,
            topology_json(&state.config, &nodes, &leases, pools, Utc::now()),
        ),
        Err(error) => render_store_error(Some(&start), error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str, role: &str, version: &str, seen_ago: i64, stopped: bool) -> ClusterNode {
        let now = Utc::now();
        ClusterNode {
            instance_id: id.into(),
            role: role.into(),
            node_name: "mail-1".into(),
            version: version.into(),
            details: json!({}),
            started_at: now - chrono::Duration::hours(1),
            last_seen_at: now - chrono::Duration::seconds(seen_ago),
            stopped_at: stopped.then_some(now),
        }
    }

    #[test]
    fn status_follows_the_heartbeat() {
        let now = Utc::now();
        assert_eq!(
            node_status(&node("a", "web", "1", 10, false), now, 90),
            "live"
        );
        assert_eq!(
            node_status(&node("a", "web", "1", 200, false), now, 90),
            "stale"
        );
        assert_eq!(
            node_status(&node("a", "web", "1", 10, true), now, 90),
            "stopped"
        );
    }

    #[test]
    fn leases_map_to_workers_and_versions_are_compared_among_live_nodes() {
        let config = camelmailer_config::Config::default();
        let nodes = vec![
            node("worker-1", "worker", "0.15.0", 5, false),
            node("smtp-1", "smtp", "0.14.0", 5, false),
            node("worker-old", "worker", "0.13.0", 5000, false),
        ];
        let leases = vec![
            NodeLeases {
                locked_by: "worker-1".into(),
                queued_messages: 3,
                webhook_requests: 1,
                lifecycle_events: 0,
                oldest_lock_at: Some(Utc::now()),
            },
            NodeLeases {
                locked_by: "worker-gone".into(),
                queued_messages: 2,
                ..NodeLeases::default()
            },
        ];
        let doc = topology_json(&config, &nodes, &leases, vec![], Utc::now());
        assert_eq!(doc["nodes"][0]["leases"]["queued_messages"], 3);
        assert_eq!(doc["nodes"][1]["leases"], Value::Null);
        assert_eq!(doc["nodes"][2]["status"], "stale");
        assert_eq!(doc["unregistered_leases"][0]["locked_by"], "worker-gone");
        assert_eq!(doc["mixed_versions"], true, "0.15.0 and 0.14.0 are live");
        assert_eq!(doc["versions"], json!(["0.14.0", "0.15.0"]));
    }
}
