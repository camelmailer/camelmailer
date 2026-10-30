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
    stale_lock_days: i32,
}

impl PgLifecycleQueue {
    pub fn new(pool: PgPool, stale_lock_days: i32) -> Self {
        Self {
            pool,
            stale_lock_days: stale_lock_days.max(1),
        }
    }

    /// Lock and return the next ready event, reclaiming stale locks.
    pub async fn dequeue(&self, worker_id: &str) -> Result<Option<LifecycleEventRow>, sqlx::Error> {
        let row = sqlx::query(
            "UPDATE lifecycle_events SET locked_by = $1, locked_at = now()
             WHERE id = (
                 SELECT id FROM lifecycle_events
                 WHERE (locked_by IS NULL
                        OR locked_at < now() - make_interval(days => $2::int))
                   AND (retry_after IS NULL OR retry_after <= now())
                 ORDER BY id
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING id, uuid, event, payload, attempts",
        )
        .bind(worker_id)
        .bind(self.stale_lock_days)
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
