//! Webhook delivery queue + tenant-scoped audit log.

use crate::pg_store::set_tenant_context;
use camelmailer_core::Id;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookRequestRow {
    pub id: i64,
    pub server_id: Id,
    pub webhook_id: Id,
    pub uuid: String,
    pub event: String,
    pub url: String,
    pub payload: String,
    pub sign: bool,
    pub attempts: i32,
    /// Extra HTTP headers snapshotted from the webhook at enqueue time.
    /// Values are secrets — never log them.
    pub headers: std::collections::BTreeMap<String, String>,
}

fn request_from_row(row: &PgRow) -> WebhookRequestRow {
    WebhookRequestRow {
        id: row.get("id"),
        server_id: row.get::<i64, _>("server_id") as Id,
        webhook_id: row.get::<i64, _>("webhook_id") as Id,
        uuid: row.get("uuid"),
        event: row.get("event"),
        url: row.get("url"),
        payload: row.get("payload"),
        sign: row.get("sign"),
        attempts: row.get("attempts"),
        headers: serde_json::from_value(row.get::<serde_json::Value, _>("headers"))
            .unwrap_or_default(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebhookLogEntry {
    pub id: i64,
    pub event: String,
    pub url: String,
    pub attempt: i32,
    pub status_code: Option<i32>,
    pub success: bool,
    pub response_body: Option<String>,
}

fn log_entry_from_row(row: &PgRow) -> WebhookLogEntry {
    WebhookLogEntry {
        id: row.get("id"),
        event: row.get("event"),
        url: row.get("url"),
        attempt: row.get("attempt"),
        status_code: row.get("status_code"),
        success: row.get("success"),
        response_body: row.get("response_body"),
    }
}

#[derive(Clone)]
pub struct PgWebhookQueue {
    pool: PgPool,
}

impl PgWebhookQueue {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue(
        &self,
        server_id: Id,
        webhook_id: Id,
        uuid: &str,
        event: &str,
        url: &str,
        payload: &str,
        sign: bool,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<i64, sqlx::Error> {
        let headers = serde_json::to_value(headers).unwrap_or_default();
        let row = sqlx::query(
            "INSERT INTO webhook_requests
                 (server_id, webhook_id, uuid, event, url, payload, sign, headers)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(server_id as i64)
        .bind(webhook_id as i64)
        .bind(uuid)
        .bind(event)
        .bind(url)
        .bind(payload)
        .bind(sign)
        .bind(&headers)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get("id"))
    }

    pub async fn dequeue(&self, worker_id: &str) -> Result<Option<WebhookRequestRow>, sqlx::Error> {
        let row = sqlx::query(
            "UPDATE webhook_requests SET locked_by = $1, locked_at = now()
             WHERE id = (
                 SELECT id FROM webhook_requests
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
        Ok(row.as_ref().map(request_from_row))
    }

    pub async fn complete(&self, id: i64) -> Result<(), sqlx::Error> {
        sqlx::query("DELETE FROM webhook_requests WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Reschedule with exponential backoff (`2^attempts` minutes, capped).
    pub async fn retry(&self, id: i64, attempts: i32) -> Result<(), sqlx::Error> {
        let minutes = 2_i64.pow(attempts.min(10) as u32).min(24 * 60);
        sqlx::query(
            "UPDATE webhook_requests
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

    /// Record one delivery attempt in the tenant-scoped audit log.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_attempt(
        &self,
        request: &WebhookRequestRow,
        attempt: i32,
        status_code: Option<i32>,
        success: bool,
        response_body: &str,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        set_tenant_context(&mut tx, request.server_id).await?;
        sqlx::query(
            "INSERT INTO webhook_request_log
                 (server_id, webhook_id, uuid, event, url, attempt, status_code, success, response_body)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(request.server_id as i64)
        .bind(request.webhook_id as i64)
        .bind(&request.uuid)
        .bind(&request.event)
        .bind(&request.url)
        .bind(attempt)
        .bind(status_code)
        .bind(success)
        .bind(&response_body[..response_body.len().min(2048)])
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn log_for_server(&self, server_id: Id) -> Result<Vec<WebhookLogEntry>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        set_tenant_context(&mut tx, server_id).await?;
        let rows = sqlx::query("SELECT * FROM webhook_request_log ORDER BY id")
            .fetch_all(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(rows.iter().map(log_entry_from_row).collect())
    }

    pub async fn queue_size(&self) -> Result<i64, sqlx::Error> {
        let row = sqlx::query("SELECT count(*) AS c FROM webhook_requests")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.get("c"))
    }

    /// Test helper: make every queued request immediately ready.
    pub async fn clear_backoff(&self) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE webhook_requests SET retry_after = NULL, locked_by = NULL")
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
