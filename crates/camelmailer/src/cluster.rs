//! Cluster topology: each process registers itself in `cluster_nodes` and
//! refreshes its entry on a heartbeat (`cluster.heartbeat_interval_seconds`),
//! so the infrastructure dashboard can show which nodes run, in which role
//! and version, and which worker holds which queue leases.
//!
//! Registration is best effort: a failure is logged and retried on the next
//! beat, and never stops the process.

use camelmailer_core::{AdminStore, NodeRegistration};
use std::sync::Arc;
use std::time::Duration;

/// Entries not seen for this many days are removed.
pub const PRUNE_AFTER_DAYS: u32 = 7;

/// Prune once per this many beats (hourly at the default 30 s interval).
const PRUNE_EVERY_BEATS: u64 = 120;

/// The registration of one process in `role` (`web`, `smtp` or `worker`).
pub fn registration(
    cluster: &camelmailer_config::Cluster,
    instance_id: String,
    role: &str,
    details: serde_json::Value,
) -> NodeRegistration {
    let mut details = details;
    if let Some(object) = details.as_object_mut() {
        object.insert(
            "public_addresses".into(),
            serde_json::json!(cluster.public_addresses),
        );
    }
    NodeRegistration {
        instance_id,
        role: role.to_string(),
        node_name: cluster.effective_node_name(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        details,
    }
}

/// A fresh instance id for a process that has none of its own.
pub fn instance_id(role: &str) -> String {
    format!("{role}-{}", camelmailer_core::token::generate_token(6))
}

/// Register `node` once, warning when the previous beat worked and this one
/// did not (and once more when it works again), so a database outage does
/// not flood the log.
async fn beat(store: &dyn AdminStore, node: &NodeRegistration, healthy: &mut bool) {
    match store.register_node(node).await {
        Ok(()) => {
            if !*healthy {
                tracing::info!(instance_id = %node.instance_id, role = %node.role, "cluster registration works again");
            }
            *healthy = true;
        }
        Err(error) => {
            if *healthy {
                tracing::warn!(%error, instance_id = %node.instance_id, role = %node.role, "could not register this process in the cluster topology; retrying on the next heartbeat");
            }
            *healthy = false;
        }
    }
}

/// Register `node` now and every `interval` until the task is aborted;
/// prune long-gone entries hourly.
pub fn spawn_heartbeat(
    store: Arc<dyn AdminStore>,
    node: NodeRegistration,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut healthy = true;
        let mut beats: u64 = 0;
        loop {
            ticker.tick().await;
            beat(store.as_ref(), &node, &mut healthy).await;
            if beats.is_multiple_of(PRUNE_EVERY_BEATS) {
                if let Err(error) = store.prune_cluster_nodes(PRUNE_AFTER_DAYS).await {
                    tracing::debug!(%error, "could not prune the cluster topology");
                }
            }
            beats = beats.wrapping_add(1);
        }
    })
}

/// Stop the heartbeat and mark the process stopped.
pub async fn stop(
    store: &dyn AdminStore,
    heartbeat: tokio::task::JoinHandle<()>,
    node: &NodeRegistration,
) {
    heartbeat.abort();
    let _ = heartbeat.await;
    if let Err(error) = store.mark_node_stopped(&node.instance_id, &node.role).await {
        tracing::warn!(%error, "could not mark this process stopped in the cluster topology");
    }
}

/// Run `process` registered in the cluster topology: register now and on
/// every heartbeat while it runs, and mark the entry stopped once it
/// returns (a graceful shutdown or an error).
pub async fn registered<T>(
    store: Arc<dyn AdminStore>,
    node: NodeRegistration,
    interval: Duration,
    process: impl std::future::Future<Output = T>,
) -> T {
    let heartbeat = spawn_heartbeat(store.clone(), node.clone(), interval);
    let output = process.await;
    stop(store.as_ref(), heartbeat, &node).await;
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use camelmailer_core::MemoryStore;

    #[tokio::test]
    async fn a_process_registers_beats_and_stops() {
        let store: Arc<dyn AdminStore> = Arc::new(MemoryStore::new());
        let cluster = camelmailer_config::Cluster {
            node_name: "mail-1".into(),
            public_addresses: vec!["49.13.35.233".into()],
            ..Default::default()
        };
        let node = registration(
            &cluster,
            "worker-abc".into(),
            "worker",
            serde_json::json!({ "threads": 4 }),
        );
        assert_eq!(node.node_name, "mail-1");
        assert_eq!(node.details["threads"], 4);
        assert_eq!(node.details["public_addresses"][0], "49.13.35.233");

        let handle = spawn_heartbeat(store.clone(), node.clone(), Duration::from_millis(20));
        tokio::time::sleep(Duration::from_millis(30)).await;
        let first = store.list_cluster_nodes().await.unwrap();
        assert_eq!(first.len(), 1);
        tokio::time::sleep(Duration::from_millis(60)).await;
        let later = store.list_cluster_nodes().await.unwrap();
        assert!(
            later[0].last_seen_at > first[0].last_seen_at,
            "heartbeat refreshes"
        );
        assert_eq!(later[0].started_at, first[0].started_at);

        stop(store.as_ref(), handle, &node).await;
        let stopped = store.list_cluster_nodes().await.unwrap();
        assert!(stopped[0].stopped_at.is_some());
    }

    #[tokio::test]
    async fn a_registered_process_is_marked_stopped_when_it_returns() {
        let store: Arc<dyn AdminStore> = Arc::new(MemoryStore::new());
        let node = registration(
            &camelmailer_config::Cluster::default(),
            "smtp-abc".into(),
            "smtp",
            serde_json::json!({}),
        );
        let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
        let watched = store.clone();
        let task = tokio::spawn(registered(
            store.clone(),
            node,
            Duration::from_millis(20),
            async move {
                let _ = stop_rx.await;
                42
            },
        ));
        tokio::time::sleep(Duration::from_millis(40)).await;
        let running = watched.list_cluster_nodes().await.unwrap();
        assert_eq!(running.len(), 1);
        assert!(running[0].stopped_at.is_none(), "live while it runs");
        stop_tx.send(()).unwrap();
        assert_eq!(task.await.unwrap(), 42);
        let stopped = watched.list_cluster_nodes().await.unwrap();
        assert!(stopped[0].stopped_at.is_some(), "stopped once it returned");
    }

    #[test]
    fn instance_ids_carry_the_role() {
        assert!(instance_id("smtp").starts_with("smtp-"));
        assert_ne!(instance_id("web"), instance_id("web"));
    }
}
