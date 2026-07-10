//! Click/open tracking token storage. Tokens are resolved via a
//! cross-tenant lookup table (the public endpoints are unauthenticated and
//! only carry a token); the recorded click/load rows land in the
//! RLS-protected `link_clicks` / `loads` tables under the resolved tenant.

use crate::pg_store::{set_tenant_context, PgStore};
use async_trait::async_trait;
use camelmailer_core::{token, Id, StoreError, TrackingStore, TrackingTarget};
use sqlx::Row;

impl PgStore {
    /// Register a click-tracking token for a message's link and return it.
    pub async fn create_click_token(
        &self,
        server_id: Id,
        message_id: i64,
        link_id: i64,
        target_url: &str,
    ) -> Result<String, sqlx::Error> {
        let tracking_token = token::generate_token(24);
        sqlx::query(
            "INSERT INTO tracking_tokens (token, kind, server_id, message_id, link_id, target_url)
             VALUES ($1, 'click', $2, $3, $4, $5)",
        )
        .bind(&tracking_token)
        .bind(server_id as i64)
        .bind(message_id)
        .bind(link_id)
        .bind(target_url)
        .execute(self.pool())
        .await?;
        Ok(tracking_token)
    }

    /// Register an open-tracking (pixel) token for a message and return it.
    pub async fn create_open_token(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<String, sqlx::Error> {
        let tracking_token = token::generate_token(24);
        sqlx::query(
            "INSERT INTO tracking_tokens (token, kind, server_id, message_id)
             VALUES ($1, 'open', $2, $3)",
        )
        .bind(&tracking_token)
        .bind(server_id as i64)
        .bind(message_id)
        .execute(self.pool())
        .await?;
        Ok(tracking_token)
    }
}

#[async_trait]
impl TrackingStore for PgStore {
    async fn resolve_token(&self, token: &str) -> Result<Option<TrackingTarget>, StoreError> {
        sqlx::query("SELECT * FROM tracking_tokens WHERE token = $1")
            .bind(token)
            .fetch_optional(self.pool())
            .await
            .map(|row| {
                row.map(|row| TrackingTarget {
                    kind: row.get("kind"),
                    server_id: row.get::<i64, _>("server_id") as Id,
                    message_id: row.get("message_id"),
                    link_id: row.get::<Option<i64>, _>("link_id").map(|id| id as Id),
                    target_url: row.get("target_url"),
                })
            })
            .map_err(|error| StoreError::Other(error.to_string()))
    }

    async fn record_click(
        &self,
        target: &TrackingTarget,
        ip_address: &str,
        user_agent: &str,
    ) -> Result<(), StoreError> {
        let Some(link_id) = target.link_id else {
            return Ok(());
        };
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?;
        set_tenant_context(&mut tx, target.server_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?;
        sqlx::query(
            "INSERT INTO link_clicks (server_id, link_id, ip_address, user_agent)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(target.server_id as i64)
        .bind(link_id as i64)
        .bind(ip_address)
        .bind(user_agent)
        .execute(&mut *tx)
        .await
        .map_err(|e| StoreError::Other(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }

    async fn record_open(
        &self,
        target: &TrackingTarget,
        ip_address: &str,
        user_agent: &str,
    ) -> Result<(), StoreError> {
        let mut tx = self
            .pool()
            .begin()
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?;
        set_tenant_context(&mut tx, target.server_id)
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?;
        sqlx::query(
            "INSERT INTO loads (server_id, message_id, ip_address, user_agent)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(target.server_id as i64)
        .bind(target.message_id)
        .bind(ip_address)
        .bind(user_agent)
        .execute(&mut *tx)
        .await
        .map_err(|e| StoreError::Other(e.to_string()))?;
        tx.commit()
            .await
            .map_err(|e| StoreError::Other(e.to_string()))?;
        Ok(())
    }
}
