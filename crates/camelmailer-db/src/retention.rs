//! The data retention cleanup, run by the worker's housekeeping.
//!
//! Every delete here is batched: a statement touches at most `batch` rows
//! and commits before the next one, so a large backlog (the first run after
//! retention is switched on) never holds one long transaction, one huge
//! lock set or one giant WAL burst. Tenant tables under row-level security
//! are processed per server inside that server's tenant context.

use crate::pg_store::{retention_overrides_from_row, set_tenant_context, RETENTION_COLUMNS};
use crate::PgStore;
use camelmailer_core::{Id, RetentionOverrides};
use chrono::{DateTime, Utc};
use sqlx::Row;

/// A server with its organization's retention overrides.
#[derive(Debug, Clone)]
pub struct RetentionTarget {
    pub server_id: Id,
    pub organization_id: Id,
    pub overrides: RetentionOverrides,
}

/// What one cleanup pass removed, per data class.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub message_content_purged: u64,
    pub messages_deleted: u64,
    pub webhook_log_deleted: u64,
    pub dmarc_reports_deleted: u64,
    pub auth_events_deleted: u64,
    pub api_requests_deleted: u64,
    pub expired_tokens_deleted: u64,
}

impl RetentionReport {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

impl PgStore {
    /// Every server with its organization's retention overrides.
    pub async fn retention_targets(&self) -> Result<Vec<RetentionTarget>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT s.id AS server_id, o.id AS organization_id, {}
             FROM servers s JOIN organizations o ON o.id = s.organization_id
             ORDER BY s.id",
            RETENTION_COLUMNS
                .split(", ")
                .map(|column| format!("o.{}", column.trim()))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|row| RetentionTarget {
                server_id: row.get::<i64, _>("server_id") as Id,
                organization_id: row.get::<i64, _>("organization_id") as Id,
                overrides: retention_overrides_from_row(row),
            })
            .collect())
    }

    /// Empty `raw_message` of a server's messages created before `cutoff`,
    /// in batches. Messages still waiting in the delivery queue keep their
    /// content (the worker needs it to send them).
    pub async fn purge_message_content(
        &self,
        server_id: Id,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut total = 0;
        loop {
            let mut tx = self.pool().begin().await?;
            set_tenant_context(&mut tx, server_id).await?;
            let purged = sqlx::query(
                "UPDATE messages SET raw_message = ''::bytea, content_purged_at = now()
                 WHERE id IN (
                     SELECT m.id FROM messages m
                     WHERE m.created_at < $1 AND m.content_purged_at IS NULL
                       AND NOT EXISTS (
                           SELECT 1 FROM queued_messages q
                           WHERE q.server_id = $2 AND q.message_id = m.id)
                     LIMIT $3)",
            )
            .bind(cutoff)
            .bind(server_id as i64)
            .bind(batch)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            tx.commit().await?;
            total += purged;
            if (purged as i64) < batch {
                return Ok(total);
            }
        }
    }

    /// Delete a server's messages created before `cutoff` with everything
    /// hanging off them (deliveries, opens, links and clicks, tracking
    /// tokens, queue entries, share links), in batches.
    pub async fn prune_message_metadata(
        &self,
        server_id: Id,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut total = 0;
        loop {
            let mut tx = self.pool().begin().await?;
            set_tenant_context(&mut tx, server_id).await?;
            let expired: Vec<i64> =
                sqlx::query("SELECT id FROM messages WHERE created_at < $1 ORDER BY id LIMIT $2")
                    .bind(cutoff)
                    .bind(batch)
                    .fetch_all(&mut *tx)
                    .await?
                    .iter()
                    .map(|row| row.get::<i64, _>("id"))
                    .collect();
            if expired.is_empty() {
                tx.commit().await?;
                return Ok(total);
            }
            sqlx::query("UPDATE messages SET bounce_for_id = NULL WHERE bounce_for_id = ANY($1)")
                .bind(&expired)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "DELETE FROM link_clicks
                 WHERE link_id IN (SELECT id FROM links WHERE message_id = ANY($1))",
            )
            .bind(&expired)
            .execute(&mut *tx)
            .await?;
            for table in ["links", "loads", "deliveries"] {
                sqlx::query(&format!("DELETE FROM {table} WHERE message_id = ANY($1)"))
                    .bind(&expired)
                    .execute(&mut *tx)
                    .await?;
            }
            for table in ["tracking_tokens", "queued_messages", "message_shares"] {
                sqlx::query(&format!(
                    "DELETE FROM {table} WHERE server_id = $1 AND message_id = ANY($2)"
                ))
                .bind(server_id as i64)
                .bind(&expired)
                .execute(&mut *tx)
                .await?;
            }
            let removed = sqlx::query("DELETE FROM messages WHERE id = ANY($1)")
                .bind(&expired)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            tx.commit().await?;
            total += removed;
            if (expired.len() as i64) < batch {
                return Ok(total);
            }
        }
    }

    /// Delete rows of a tenant table older than `cutoff`, in batches, inside
    /// the server's tenant context.
    async fn prune_tenant_table(
        &self,
        server_id: Id,
        table: &str,
        column: &str,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut total = 0;
        loop {
            let mut tx = self.pool().begin().await?;
            set_tenant_context(&mut tx, server_id).await?;
            if table == "dmarc_reports" {
                // The report rows are under row-level security too; delete
                // them explicitly rather than relying on the cascade.
                sqlx::query(
                    "DELETE FROM dmarc_report_records WHERE report_id IN (
                         SELECT id FROM dmarc_reports
                         WHERE server_id = $1 AND received_at < $2
                         ORDER BY id LIMIT $3)",
                )
                .bind(server_id as i64)
                .bind(cutoff)
                .bind(batch)
                .execute(&mut *tx)
                .await?;
            }
            let removed = sqlx::query(&format!(
                "DELETE FROM {table} WHERE id IN (
                     SELECT id FROM {table} WHERE server_id = $1 AND {column} < $2
                     ORDER BY id LIMIT $3)"
            ))
            .bind(server_id as i64)
            .bind(cutoff)
            .bind(batch)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            tx.commit().await?;
            total += removed;
            if (removed as i64) < batch {
                return Ok(total);
            }
        }
    }

    /// The webhook delivery log of one server, older than `cutoff`.
    pub async fn prune_webhook_log(
        &self,
        server_id: Id,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        self.prune_tenant_table(
            server_id,
            "webhook_request_log",
            "created_at",
            cutoff,
            batch,
        )
        .await
    }

    /// DMARC aggregate reports of one server received before `cutoff`.
    pub async fn prune_dmarc_reports(
        &self,
        server_id: Id,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        self.prune_tenant_table(server_id, "dmarc_reports", "received_at", cutoff, batch)
            .await
    }

    /// Delete rows of an installation-wide table older than `cutoff`.
    async fn prune_global_table(
        &self,
        table: &str,
        id_column: &str,
        time_column: &str,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut total = 0;
        loop {
            let removed = sqlx::query(&format!(
                "DELETE FROM {table} WHERE {id_column} IN (
                     SELECT {id_column} FROM {table} WHERE {time_column} < $1 LIMIT $2)"
            ))
            .bind(cutoff)
            .bind(batch)
            .execute(self.pool())
            .await?
            .rows_affected();
            total += removed;
            if (removed as i64) < batch {
                return Ok(total);
            }
        }
    }

    /// The authentication audit log, older than `cutoff`.
    pub async fn prune_auth_events(
        &self,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        self.prune_global_table("auth_events", "id", "created_at", cutoff, batch)
            .await
    }

    /// The per-server API request log, older than `cutoff`.
    pub async fn prune_api_request_log(
        &self,
        cutoff: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        self.prune_global_table("api_requests", "id", "created_at", cutoff, batch)
            .await
    }

    /// Authentication tokens and ceremony state past their expiry: sessions,
    /// password resets, invitations, message share links, OIDC, WebAuthn
    /// and SAML state. Always runs; an expired token is useless and only
    /// keeps personal data around.
    pub async fn prune_expired_auth_tokens(
        &self,
        now: DateTime<Utc>,
        batch: i64,
    ) -> Result<u64, sqlx::Error> {
        let mut total = 0;
        for (table, id_column) in [
            ("auth_sessions", "id"),
            ("password_resets", "id"),
            ("invitations", "id"),
            ("message_shares", "id"),
            ("oidc_states", "state"),
            ("webauthn_states", "key"),
            ("saml_requests", "request_id"),
            ("saml_assertions", "assertion_id"),
        ] {
            total += self
                .prune_global_table(table, id_column, "expires_at", now, batch)
                .await?;
        }
        Ok(total)
    }
}
