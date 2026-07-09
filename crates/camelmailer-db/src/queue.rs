//! The delivery queue — the port of Postal's main-DB `QueuedMessage` model
//! and the locking logic in `app/lib/message_dequeuer`.
//!
//! The queue itself is cross-tenant (the worker's work list); message
//! *content* is only ever loaded by entering the owning server's RLS tenant
//! context. Locking uses `FOR UPDATE SKIP LOCKED` so multiple workers can
//! dequeue concurrently without stepping on each other.

use camelmailer_core::Id;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

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

#[derive(Clone)]
pub struct PgQueue {
    pool: PgPool,
}

impl PgQueue {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
    pub async fn dequeue(&self, worker_id: &str) -> Result<Option<QueuedMessageRow>, sqlx::Error> {
        let row = sqlx::query(
            "UPDATE queued_messages SET locked_by = $1, locked_at = now()
             WHERE id = (
                 SELECT id FROM queued_messages
                 WHERE locked_by IS NULL AND (retry_after IS NULL OR retry_after <= now())
                 ORDER BY id
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING *",
        )
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(queued_from_row))
    }

    /// Delivery finished (successfully or terminally) — remove from queue.
    pub async fn complete(&self, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM queued_messages WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Soft failure — unlock and reschedule with exponential backoff
    /// (`2^attempts` minutes, capped at one day).
    pub async fn retry(&self, id: i64, attempts: i32) -> Result<(), sqlx::Error> {
        let minutes = 2_i64.pow(attempts.min(10) as u32).min(24 * 60);
        sqlx::query(
            "UPDATE queued_messages
             SET locked_by = NULL, locked_at = NULL, attempts = attempts + 1,
                 retry_after = now() + make_interval(mins => $2::int)
             WHERE id = $1",
        )
        .bind(id)
        .bind(minutes as i32)
        .execute(&self.pool)
        .await?;
        Ok(())
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
