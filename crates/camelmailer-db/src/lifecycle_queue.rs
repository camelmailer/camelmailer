//! The installation lifecycle-event outbox (`lifecycle_events`).
//!
//! Rows are written by the web server (organization created/deleted, user
//! registered) and by the message insert transaction (usage thresholds). The
//! worker locks one ready row at a time with `SKIP LOCKED`, delivers it to
//! every configured URL, and deletes it on success or after the last
//! attempt; a failure reschedules it with the same exponential backoff as
//! message webhooks.

use sqlx::{PgPool, Row};

/// One queued lifecycle event.
#[derive(Debug, Clone)]
pub struct LifecycleEventRow {
    pub id: i64,
    pub uuid: String,
    pub event: String,
    /// The exact JSON body to send.
    pub payload: String,
    pub attempts: i32,
}

#[derive(Clone)]
pub struct PgLifecycleQueue {
    pool: PgPool,
    stale_lock_seconds: i64,
}

impl PgLifecycleQueue {
    /// A queue whose stale-lock window is `stale_lock_days` (at least one).
    pub fn new(pool: PgPool, stale_lock_days: i32) -> Self {
        Self {
            pool,
            stale_lock_seconds: crate::queue::days_to_lease_seconds(stale_lock_days),
        }
    }

    /// A queue whose stale-lock window is in seconds (at least 90).
    pub fn with_stale_lock_seconds(pool: PgPool, stale_lock_seconds: i64) -> Self {
        Self {
            pool,
            stale_lock_seconds: crate::queue::clamp_lease_seconds(stale_lock_seconds),
        }
    }

    /// Keep the locks `worker_id` holds on `ids` fresh.
    pub async fn heartbeat(&self, worker_id: &str, ids: &[i64]) -> Result<u64, sqlx::Error> {
        crate::queue::heartbeat_rows(&self.pool, "lifecycle_events", worker_id, ids).await
    }

    /// Hand back every event `worker_id` holds without counting an attempt.
    pub async fn release_all(&self, worker_id: &str) -> Result<u64, sqlx::Error> {
        crate::queue::release_rows(&self.pool, "lifecycle_events", worker_id).await
    }

    /// Lock and return the next ready event, reclaiming stale locks.
    pub async fn dequeue(&self, worker_id: &str) -> Result<Option<LifecycleEventRow>, sqlx::Error> {
        let row = sqlx::query(
            "UPDATE lifecycle_events SET locked_by = $1, locked_at = now()
             WHERE id = (
                 SELECT id FROM lifecycle_events
                 WHERE (locked_by IS NULL
                        OR locked_at < now() - make_interval(secs => $2::bigint))
                   AND (retry_after IS NULL OR retry_after <= now())
                 ORDER BY id
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING id, uuid, event, payload, attempts",
        )
        .bind(worker_id)
        .bind(self.stale_lock_seconds)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|row| LifecycleEventRow {
            id: row.get("id"),
            uuid: row.get("uuid"),
            event: row.get("event"),
            payload: row.get("payload"),
            attempts: row.get("attempts"),
        }))
    }

    /// Delivered (or given up): remove the row.
    pub async fn complete(&self, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM lifecycle_events WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Reschedule with exponential backoff (`2^attempts` minutes, capped).
    pub async fn retry(&self, id: i64, attempts: i32, error: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE lifecycle_events
             SET locked_by = NULL, locked_at = NULL, attempts = attempts + 1,
                 retry_after = now() + make_interval(mins => $2::int), last_error = $3
             WHERE id = $1",
        )
        .bind(id)
        .bind(crate::queue::retry_delay_minutes(attempts))
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop every queued event. Used by housekeeping when no lifecycle URL
    /// is configured, so the outbox cannot grow on an installation that
    /// never forwards events.
    pub async fn discard_all(&self) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query("DELETE FROM lifecycle_events")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    /// Every queued event, oldest first (tests and diagnostics).
    pub async fn pending(&self) -> Result<Vec<LifecycleEventRow>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, uuid, event, payload, attempts FROM lifecycle_events ORDER BY id",
        )
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| LifecycleEventRow {
                id: row.get("id"),
                uuid: row.get("uuid"),
                event: row.get("event"),
                payload: row.get("payload"),
                attempts: row.get("attempts"),
            })
            .collect())
    }
}
