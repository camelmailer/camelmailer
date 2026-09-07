//! The delivery queue — the port of Postal's main-DB `QueuedMessage` model
//! and the locking logic in `app/lib/message_dequeuer`.
//!
//! The queue itself is cross-tenant (the worker's work list); message
//! *content* is only ever loaded by entering the owning server's RLS tenant
//! context. Locking uses `FOR UPDATE SKIP LOCKED` so multiple workers can
//! dequeue concurrently without stepping on each other.

use camelmailer_core::Id;
use sqlx::postgres::PgRow;
use sqlx::{Executor, PgPool, Postgres, Row};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessageRow {
    pub id: i64,
    pub message_id: i64,
    pub server_id: Id,
    pub domain: String,
    pub attempts: i32,
}

fn queued_from_row(row: &PgRow) -> QueuedMessageRow {
    QueuedMessageRow {
        id: row.get("id"),
        message_id: row.get("message_id"),
        server_id: row.get::<i64, _>("server_id") as Id,
        domain: row.get("domain"),
        attempts: row.get("attempts"),
    }
}

/// Preserve the existing retry schedule: 1 minute initially, up to 1024 minutes.
pub(crate) fn retry_delay_minutes(attempts: i32) -> i32 {
    2_i32.pow(attempts.clamp(0, 10) as u32)
}

/// Queue mutations accept either a pool or the delivery's open transaction.
/// Delivery callers additionally constrain the row to its message and tenant.
pub(crate) async fn complete_message<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    id: i64,
    owner: Option<(i64, Id)>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM queued_messages WHERE id = $1
         AND ($2::bigint IS NULL OR message_id = $2)
         AND ($3::bigint IS NULL OR server_id = $3)",
    )
    .bind(id)
    .bind(owner.map(|(message_id, _)| message_id))
    .bind(owner.map(|(_, server_id)| server_id as i64))
    .execute(executor)
    .await?;
    Ok(())
}

pub(crate) async fn retry_message<'e>(
    executor: impl Executor<'e, Database = Postgres>,
    id: i64,
    attempts: i32,
    owner: Option<(i64, Id)>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE queued_messages
         SET locked_by = NULL, locked_at = NULL, attempts = attempts + 1,
             retry_after = now() + make_interval(mins => $2::int)
         WHERE id = $1
         AND ($3::bigint IS NULL OR message_id = $3)
         AND ($4::bigint IS NULL OR server_id = $4)",
    )
    .bind(id)
    .bind(retry_delay_minutes(attempts))
    .bind(owner.map(|(message_id, _)| message_id))
    .bind(owner.map(|(_, server_id)| server_id as i64))
    .execute(executor)
    .await?;
    Ok(())
}

/// Default stale-lock window (days) when a caller does not configure one.
/// Matches `camelmailer.queued_message_lock_stale_days`'s default.
const DEFAULT_STALE_LOCK_DAYS: i32 = 1;

#[derive(Clone)]
pub struct PgQueue {
    pool: PgPool,
    /// A message locked (`locked_by` set) longer ago than this is treated as
    /// abandoned by a crashed worker and re-dequeued.
    stale_lock_days: i32,
}

impl PgQueue {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            stale_lock_days: DEFAULT_STALE_LOCK_DAYS,
        }
    }

    /// Like [`PgQueue::new`] but with an explicit stale-lock window (from
    /// `camelmailer.queued_message_lock_stale_days`). A value `<= 0` is
    /// clamped to 1 day so a misconfiguration can never reclaim actively
    /// locked messages.
    pub fn with_stale_lock_days(pool: PgPool, stale_lock_days: i32) -> Self {
        Self {
            pool,
            stale_lock_days: stale_lock_days.max(1),
        }
    }

    pub async fn enqueue(
        &self,
        message_id: i64,
        server_id: Id,
        domain: &str,
    ) -> Result<i64, sqlx::Error> {
        let row = sqlx::query(
            "INSERT INTO queued_messages (message_id, server_id, domain)
             VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(message_id)
        .bind(server_id as i64)
        .bind(domain)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("id"))
    }

    /// Lock and return the next ready queued message, if any.
    ///
    /// A row is ready when it is unlocked, *or* when its lock is stale — held
    /// since before `now() - queued_message_lock_stale_days`, i.e. by a worker
    /// that crashed mid-delivery. Without the stale branch such a row would
    /// stay "sending" forever; with it, a surviving worker reclaims it.
    pub async fn dequeue(&self, worker_id: &str) -> Result<Option<QueuedMessageRow>, sqlx::Error> {
        let row = sqlx::query(
            "UPDATE queued_messages SET locked_by = $1, locked_at = now()
             WHERE id = (
                 SELECT id FROM queued_messages
                 WHERE (locked_by IS NULL
                        OR locked_at < now() - make_interval(days => $2::int))
                   AND (retry_after IS NULL OR retry_after <= now())
                 ORDER BY id
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING *",
        )
        .bind(worker_id)
        .bind(self.stale_lock_days)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(queued_from_row))
    }

    /// Delivery finished (successfully or terminally) — remove from queue.
    pub async fn complete(&self, id: i64) -> Result<(), sqlx::Error> {
        complete_message(&self.pool, id, None).await
    }

    /// Soft failure — unlock and reschedule with exponential backoff.
    pub async fn retry(&self, id: i64, attempts: i32) -> Result<(), sqlx::Error> {
        retry_message(&self.pool, id, attempts, None).await
    }

    pub async fn queue_size(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT count(*) AS c FROM queued_messages")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("c"))
    }

    /// Test helper: make every queued message immediately ready.
    pub async fn clear_backoff(&self) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE queued_messages SET retry_after = NULL, locked_by = NULL")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
