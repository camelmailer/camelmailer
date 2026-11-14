//! The PostgreSQL implementation of [`DeliveryStatsStore`]: delivery
//! counters per source address and provider (`ip_delivery_stats`), and the
//! source of each accepted message (`message_sources`).

use async_trait::async_trait;
use camelmailer_core::{
    DeliveryKind, DeliveryStatsStore, IpComplaint, IpDeliveryRecord, IpDeliveryStat, ProviderGroup,
    StoreError,
};
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row;

use crate::pg_store::PgStore;

fn sqlx_error(error: sqlx::Error) -> StoreError {
    StoreError::Other(error.to_string())
}

fn provider_of(value: &str) -> ProviderGroup {
    ProviderGroup::parse(value).unwrap_or(ProviderGroup::Other)
}

#[async_trait]
impl DeliveryStatsStore for PgStore {
    async fn record_ip_delivery(&self, record: &IpDeliveryRecord) -> Result<(), StoreError> {
        let (sent, deferred, bounced) = match record.kind {
            DeliveryKind::Sent => (1_i64, 0_i64, 0_i64),
            DeliveryKind::Deferred => (0, 1, 0),
            DeliveryKind::Bounced => (0, 0, 1),
        };
        let mut tx = self.pool().begin().await.map_err(sqlx_error)?;
        sqlx::query(
            "INSERT INTO ip_delivery_stats
                 (day, source_ip, provider, ip_address_id, sent, deferred, bounced, policy_blocks)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (day, source_ip, provider) DO UPDATE SET
                 ip_address_id = COALESCE(EXCLUDED.ip_address_id, ip_delivery_stats.ip_address_id),
                 sent = ip_delivery_stats.sent + EXCLUDED.sent,
                 deferred = ip_delivery_stats.deferred + EXCLUDED.deferred,
                 bounced = ip_delivery_stats.bounced + EXCLUDED.bounced,
                 policy_blocks = ip_delivery_stats.policy_blocks + EXCLUDED.policy_blocks,
                 updated_at = now()",
        )
        .bind(record.day)
        .bind(&record.source_ip)
        .bind(record.provider.as_str())
        .bind(record.ip_address_id.map(|id| id as i64))
        .bind(sent)
        .bind(deferred)
        .bind(bounced)
        .bind(i64::from(record.policy_block))
        .execute(&mut *tx)
        .await
        .map_err(sqlx_error)?;
        if record.kind == DeliveryKind::Sent {
            sqlx::query(
                "INSERT INTO message_sources
                     (message_id, server_id, source_ip, ip_address_id, provider)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (message_id) DO UPDATE SET
                     source_ip = EXCLUDED.source_ip, ip_address_id = EXCLUDED.ip_address_id,
                     provider = EXCLUDED.provider, created_at = now()",
            )
            .bind(record.message_id)
            .bind(record.server_id as i64)
            .bind(&record.source_ip)
            .bind(record.ip_address_id.map(|id| id as i64))
            .bind(record.provider.as_str())
            .execute(&mut *tx)
            .await
            .map_err(sqlx_error)?;
        }
        tx.commit().await.map_err(sqlx_error)
    }

    async fn record_ip_complaint(
        &self,
        message_id: i64,
        at: DateTime<Utc>,
    ) -> Result<Option<IpComplaint>, StoreError> {
        let mut tx = self.pool().begin().await.map_err(sqlx_error)?;
        let Some(row) = sqlx::query(
            "SELECT source_ip, ip_address_id, provider FROM message_sources WHERE message_id = $1",
        )
        .bind(message_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(sqlx_error)?
        else {
            return Ok(None);
        };
        let source = IpComplaint {
            source_ip: row.get("source_ip"),
            ip_address_id: row
                .get::<Option<i64>, _>("ip_address_id")
                .map(|id| id as camelmailer_core::Id),
            provider: provider_of(row.get("provider")),
        };
        sqlx::query(
            "INSERT INTO ip_delivery_stats (day, source_ip, provider, ip_address_id, complaints)
             VALUES ($1, $2, $3, $4, 1)
             ON CONFLICT (day, source_ip, provider) DO UPDATE SET
                 complaints = ip_delivery_stats.complaints + 1, updated_at = now()",
        )
        .bind(at.date_naive())
        .bind(&source.source_ip)
        .bind(source.provider.as_str())
        .bind(source.ip_address_id.map(|id| id as i64))
        .execute(&mut *tx)
        .await
        .map_err(sqlx_error)?;
        tx.commit().await.map_err(sqlx_error)?;
        Ok(Some(source))
    }

    async fn ip_delivery_stats(&self, since: NaiveDate) -> Result<Vec<IpDeliveryStat>, StoreError> {
        let rows = sqlx::query(
            "SELECT day, source_ip, provider, ip_address_id, sent, deferred, bounced,
                    policy_blocks, complaints
             FROM ip_delivery_stats WHERE day >= $1
             ORDER BY day, source_ip, provider",
        )
        .bind(since)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(rows
            .iter()
            .map(|row| IpDeliveryStat {
                day: row.get("day"),
                source_ip: row.get("source_ip"),
                ip_address_id: row
                    .get::<Option<i64>, _>("ip_address_id")
                    .map(|id| id as camelmailer_core::Id),
                provider: provider_of(row.get("provider")),
                sent: row.get("sent"),
                deferred: row.get("deferred"),
                bounced: row.get("bounced"),
                policy_blocks: row.get("policy_blocks"),
                complaints: row.get("complaints"),
            })
            .collect())
    }

    async fn prune_delivery_stats(
        &self,
        stats_before: NaiveDate,
        sources_before: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let stats = sqlx::query("DELETE FROM ip_delivery_stats WHERE day < $1")
            .bind(stats_before)
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?
            .rows_affected();
        let sources = sqlx::query("DELETE FROM message_sources WHERE created_at < $1")
            .bind(sources_before)
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?
            .rows_affected();
        Ok(stats + sources)
    }
}
