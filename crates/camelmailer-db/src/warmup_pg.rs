//! The PostgreSQL implementation of [`WarmupStore`]: warm-up settings per
//! pool and address, the ramp state per address and provider, daily usage
//! and the decision log.

use async_trait::async_trait;
use camelmailer_core::{
    Id, ProviderGroup, StoreError, WarmupAddress, WarmupLogEntry, WarmupMode, WarmupProfile,
    WarmupState, WarmupStatus, WarmupStore,
};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::pg_store::PgStore;

fn sqlx_error(error: sqlx::Error) -> StoreError {
    StoreError::Other(error.to_string())
}

fn provider_of(value: &str) -> ProviderGroup {
    ProviderGroup::parse(value).unwrap_or_else(ProviderGroup::other)
}

fn profile_of(value: Option<serde_json::Value>) -> Option<WarmupProfile> {
    value.and_then(|v| serde_json::from_value(v).ok())
}

fn profile_json(profile: Option<WarmupProfile>) -> Option<serde_json::Value> {
    profile.and_then(|p| serde_json::to_value(p).ok())
}

fn state_from_row(row: &PgRow) -> WarmupState {
    WarmupState {
        ip_address_id: row.get::<i64, _>("ip_address_id") as Id,
        provider: provider_of(row.get("provider")),
        allowance: row.get("allowance"),
        status: WarmupStatus::parse(row.get("status")).unwrap_or(WarmupStatus::Holding),
        paused_until: row.get("paused_until"),
        evaluated_day: row.get("evaluated_day"),
        last_reason: row.get("last_reason"),
        last_brake_reason: row.get("last_brake_reason"),
        last_brake_at: row.get("last_brake_at"),
        version: row.get("version"),
    }
}

async fn insert_log(
    executor: impl sqlx::Executor<'_, Database = sqlx::Postgres>,
    entry: &WarmupLogEntry,
) -> Result<(), StoreError> {
    sqlx::query(
        "INSERT INTO ip_warmup_log
             (ip_address_id, provider, decision, reason, allowance_before, allowance_after, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(entry.ip_address_id as i64)
    .bind(entry.provider.as_ref().map(|p| p.as_str()))
    .bind(&entry.decision)
    .bind(&entry.reason)
    .bind(entry.allowance_before)
    .bind(entry.allowance_after)
    .bind(entry.created_at)
    .execute(executor)
    .await
    .map_err(sqlx_error)?;
    Ok(())
}

#[async_trait]
impl WarmupStore for PgStore {
    async fn warmup_addresses(&self) -> Result<Vec<WarmupAddress>, StoreError> {
        let rows = sqlx::query(
            "SELECT a.id, a.ipv4, a.hostname, p.id AS pool_id, p.name AS pool_name,
                    aw.mode AS address_mode, aw.profile AS address_profile,
                    pw.mode AS pool_mode, pw.profile AS pool_profile
             FROM ip_addresses a
             JOIN ip_pools p ON p.id = a.ip_pool_id
             LEFT JOIN ip_address_warmup aw ON aw.ip_address_id = a.id
             LEFT JOIN ip_pool_warmup pw ON pw.ip_pool_id = p.id
             ORDER BY p.id, a.id",
        )
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(rows
            .iter()
            .map(|row| WarmupAddress {
                ip_address_id: row.get::<i64, _>("id") as Id,
                ipv4: row.get("ipv4"),
                hostname: row.get("hostname"),
                ip_pool_id: row.get::<i64, _>("pool_id") as Id,
                pool_name: row.get("pool_name"),
                address_mode: row
                    .get::<Option<String>, _>("address_mode")
                    .and_then(|m| WarmupMode::parse(&m)),
                address_profile: profile_of(row.get("address_profile")),
                pool_mode: row
                    .get::<Option<String>, _>("pool_mode")
                    .and_then(|m| WarmupMode::parse(&m)),
                pool_profile: profile_of(row.get("pool_profile")),
            })
            .collect())
    }

    async fn set_address_warmup(
        &self,
        ip_address_id: Id,
        mode: Option<WarmupMode>,
        profile: Option<WarmupProfile>,
    ) -> Result<bool, StoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ip_addresses WHERE id = $1)")
                .bind(ip_address_id as i64)
                .fetch_one(self.pool())
                .await
                .map_err(sqlx_error)?;
        if !exists {
            return Ok(false);
        }
        match mode {
            Some(mode) => sqlx::query(
                "INSERT INTO ip_address_warmup (ip_address_id, mode, profile)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (ip_address_id) DO UPDATE SET
                     mode = EXCLUDED.mode, profile = EXCLUDED.profile, updated_at = now()",
            )
            .bind(ip_address_id as i64)
            .bind(mode.as_str())
            .bind(profile_json(profile))
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?,
            None => sqlx::query("DELETE FROM ip_address_warmup WHERE ip_address_id = $1")
                .bind(ip_address_id as i64)
                .execute(self.pool())
                .await
                .map_err(sqlx_error)?,
        };
        Ok(true)
    }

    async fn set_pool_warmup(
        &self,
        ip_pool_id: Id,
        mode: Option<WarmupMode>,
        profile: Option<WarmupProfile>,
    ) -> Result<bool, StoreError> {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM ip_pools WHERE id = $1)")
                .bind(ip_pool_id as i64)
                .fetch_one(self.pool())
                .await
                .map_err(sqlx_error)?;
        if !exists {
            return Ok(false);
        }
        match mode {
            Some(mode) => sqlx::query(
                "INSERT INTO ip_pool_warmup (ip_pool_id, mode, profile)
                 VALUES ($1, $2, $3)
                 ON CONFLICT (ip_pool_id) DO UPDATE SET
                     mode = EXCLUDED.mode, profile = EXCLUDED.profile, updated_at = now()",
            )
            .bind(ip_pool_id as i64)
            .bind(mode.as_str())
            .bind(profile_json(profile))
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?,
            None => sqlx::query("DELETE FROM ip_pool_warmup WHERE ip_pool_id = $1")
                .bind(ip_pool_id as i64)
                .execute(self.pool())
                .await
                .map_err(sqlx_error)?,
        };
        Ok(true)
    }

    async fn warmup_states(&self) -> Result<Vec<WarmupState>, StoreError> {
        let rows = sqlx::query("SELECT * FROM ip_warmup_state ORDER BY ip_address_id, provider")
            .fetch_all(self.pool())
            .await
            .map_err(sqlx_error)?;
        Ok(rows.iter().map(state_from_row).collect())
    }

    async fn save_warmup_state(
        &self,
        state: &WarmupState,
        entry: Option<&WarmupLogEntry>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await.map_err(sqlx_error)?;
        let saved = if state.version == 0 {
            sqlx::query(
                "INSERT INTO ip_warmup_state
                     (ip_address_id, provider, allowance, status, paused_until, evaluated_day,
                      last_reason, last_brake_reason, last_brake_at, version)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 1)
                 ON CONFLICT (ip_address_id, provider) DO NOTHING",
            )
        } else {
            sqlx::query(
                "UPDATE ip_warmup_state SET
                     allowance = $3, status = $4, paused_until = $5, evaluated_day = $6,
                     last_reason = $7, last_brake_reason = $8, last_brake_at = $9,
                     version = version + 1, updated_at = now()
                 WHERE ip_address_id = $1 AND provider = $2 AND version = $10",
            )
        }
        .bind(state.ip_address_id as i64)
        .bind(state.provider.as_str())
        .bind(state.allowance)
        .bind(state.status.as_str())
        .bind(state.paused_until)
        .bind(state.evaluated_day)
        .bind(&state.last_reason)
        .bind(&state.last_brake_reason)
        .bind(state.last_brake_at)
        .bind(state.version)
        .execute(&mut *tx)
        .await
        .map_err(sqlx_error)?
        .rows_affected()
            == 1;
        if !saved {
            tx.rollback().await.map_err(sqlx_error)?;
            return Ok(false);
        }
        if let Some(entry) = entry {
            insert_log(&mut *tx, entry).await?;
        }
        tx.commit().await.map_err(sqlx_error)?;
        Ok(true)
    }

    async fn try_reserve_warmup(
        &self,
        ip_address_id: Id,
        provider: ProviderGroup,
        day: NaiveDate,
        initial: &WarmupState,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await.map_err(sqlx_error)?;
        sqlx::query(
            "INSERT INTO ip_warmup_state (ip_address_id, provider, allowance, status, last_reason)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (ip_address_id, provider) DO NOTHING",
        )
        .bind(ip_address_id as i64)
        .bind(provider.as_str())
        .bind(initial.allowance)
        .bind(initial.status.as_str())
        .bind(&initial.last_reason)
        .execute(&mut *tx)
        .await
        .map_err(sqlx_error)?;
        // The state row lock serializes the reservations of every worker for
        // this address and provider, so the allowance is never overrun.
        let row = sqlx::query(
            "SELECT * FROM ip_warmup_state WHERE ip_address_id = $1 AND provider = $2 FOR UPDATE",
        )
        .bind(ip_address_id as i64)
        .bind(provider.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(sqlx_error)?;
        let state = state_from_row(&row);
        if state.paused_at(now) {
            tx.rollback().await.map_err(sqlx_error)?;
            return Ok(false);
        }
        let used: i64 = sqlx::query_scalar(
            "SELECT COALESCE((SELECT used FROM ip_warmup_usage
                              WHERE day = $1 AND ip_address_id = $2 AND provider = $3), 0)",
        )
        .bind(day)
        .bind(ip_address_id as i64)
        .bind(provider.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(sqlx_error)?;
        if state.status != WarmupStatus::Warm && used >= state.allowance {
            tx.rollback().await.map_err(sqlx_error)?;
            return Ok(false);
        }
        sqlx::query(
            "INSERT INTO ip_warmup_usage (day, ip_address_id, provider, used) VALUES ($1, $2, $3, 1)
             ON CONFLICT (day, ip_address_id, provider) DO UPDATE SET used = ip_warmup_usage.used + 1",
        )
        .bind(day)
        .bind(ip_address_id as i64)
        .bind(provider.as_str())
        .execute(&mut *tx)
        .await
        .map_err(sqlx_error)?;
        tx.commit().await.map_err(sqlx_error)?;
        Ok(true)
    }

    async fn warmup_usage(
        &self,
        day: NaiveDate,
    ) -> Result<Vec<(Id, ProviderGroup, i64)>, StoreError> {
        let rows = sqlx::query(
            "SELECT ip_address_id, provider, used FROM ip_warmup_usage WHERE day = $1
             ORDER BY ip_address_id, provider",
        )
        .bind(day)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        let mut usage: Vec<(Id, ProviderGroup, i64)> = rows
            .iter()
            .map(|row| {
                (
                    row.get::<i64, _>("ip_address_id") as Id,
                    provider_of(row.get("provider")),
                    row.get("used"),
                )
            })
            .collect();
        usage.sort();
        Ok(usage)
    }

    async fn log_warmup(&self, entry: &WarmupLogEntry) -> Result<(), StoreError> {
        insert_log(self.pool(), entry).await
    }

    async fn warmup_log(
        &self,
        ip_address_id: Option<Id>,
        limit: i64,
    ) -> Result<Vec<WarmupLogEntry>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM ip_warmup_log
             WHERE ($1::bigint IS NULL OR ip_address_id = $1)
             ORDER BY id DESC LIMIT $2",
        )
        .bind(ip_address_id.map(|id| id as i64))
        .bind(limit.max(0))
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(rows
            .iter()
            .map(|row| WarmupLogEntry {
                ip_address_id: row.get::<i64, _>("ip_address_id") as Id,
                provider: row
                    .get::<Option<String>, _>("provider")
                    .map(|p| provider_of(&p)),
                decision: row.get("decision"),
                reason: row.get("reason"),
                allowance_before: row.get("allowance_before"),
                allowance_after: row.get("allowance_after"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn prune_warmup(
        &self,
        usage_before: NaiveDate,
        log_before: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let usage = sqlx::query("DELETE FROM ip_warmup_usage WHERE day < $1")
            .bind(usage_before)
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?
            .rows_affected();
        let log = sqlx::query("DELETE FROM ip_warmup_log WHERE created_at < $1")
            .bind(log_before)
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?
            .rows_affected();
        Ok(usage + log)
    }
}
