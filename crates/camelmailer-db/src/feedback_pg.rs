//! The PostgreSQL implementation of [`FeedbackStore`]: provider feedback
//! snapshots and the fetch state of each source (migration 0055).

use async_trait::async_trait;
use camelmailer_core::feedback::{
    fbl_detail, newest_per_subject, source, subject_kind, FeedbackSnapshot, FeedbackSourceStatus,
    FeedbackStore, NewFeedbackSnapshot, ProviderSignal, Verdict,
};
use camelmailer_core::StoreError;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;

use crate::pg_store::PgStore;

fn sqlx_error(error: sqlx::Error) -> StoreError {
    StoreError::Other(error.to_string())
}

fn snapshot_from_row(row: &PgRow) -> FeedbackSnapshot {
    FeedbackSnapshot {
        source: row.get("source"),
        provider: row.get("provider"),
        subject_kind: row.get("subject_kind"),
        subject: row.get("subject"),
        data_date: row.get("data_date"),
        verdict: Verdict::parse(row.get::<String, _>("verdict").as_str()),
        detail: row.get("detail"),
        metrics: row.get("metrics"),
        fetched_at: row.get("fetched_at"),
    }
}

fn status_from_row(row: &PgRow) -> FeedbackSourceStatus {
    FeedbackSourceStatus {
        source: row.get("source"),
        enabled: row.get("enabled"),
        last_attempt_at: row.get("last_attempt_at"),
        last_success_at: row.get("last_success_at"),
        last_error: row.get("last_error"),
        consecutive_failures: row.get("consecutive_failures"),
        next_attempt_at: row.get("next_attempt_at"),
    }
}

/// The newest row per (provider, subject_kind, subject) among `rows`.
const NEWEST: &str = "SELECT DISTINCT ON (provider, subject_kind, subject) *
     FROM feedback_snapshots";

#[async_trait]
impl FeedbackStore for PgStore {
    async fn upsert_feedback_snapshots(
        &self,
        rows: &[NewFeedbackSnapshot],
    ) -> Result<usize, StoreError> {
        let mut tx = self.pool().begin().await.map_err(sqlx_error)?;
        for row in rows {
            sqlx::query(
                "INSERT INTO feedback_snapshots
                     (source, provider, subject_kind, subject, data_date, verdict, detail, metrics)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                 ON CONFLICT ON CONSTRAINT feedback_snapshots_key DO UPDATE
                 SET verdict = EXCLUDED.verdict, detail = EXCLUDED.detail,
                     metrics = EXCLUDED.metrics, fetched_at = now()",
            )
            .bind(&row.source)
            .bind(&row.provider)
            .bind(&row.subject_kind)
            .bind(&row.subject)
            .bind(row.data_date)
            .bind(row.verdict.as_str())
            .bind(&row.detail)
            .bind(&row.metrics)
            .execute(&mut *tx)
            .await
            .map_err(sqlx_error)?;
        }
        tx.commit().await.map_err(sqlx_error)?;
        Ok(rows.len())
    }

    async fn latest_feedback_snapshots(
        &self,
        source: &str,
    ) -> Result<Vec<FeedbackSnapshot>, StoreError> {
        let rows = sqlx::query(&format!(
            "{NEWEST} WHERE source = $1
             ORDER BY provider, subject_kind, subject, data_date DESC"
        ))
        .bind(source)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(newest_per_subject(
            rows.iter().map(snapshot_from_row).collect(),
        ))
    }

    async fn record_fbl_complaint(&self, provider: &str, day: NaiveDate) -> Result<(), StoreError> {
        let row = sqlx::query(
            "INSERT INTO feedback_snapshots
                 (source, provider, subject_kind, subject, data_date, verdict, detail, metrics)
             VALUES ($1, $2, $3, $2, $4, 'unknown', '', '{\"complaints\": 1}'::jsonb)
             ON CONFLICT ON CONSTRAINT feedback_snapshots_key DO UPDATE
             SET metrics = jsonb_build_object('complaints',
                     COALESCE((feedback_snapshots.metrics->>'complaints')::bigint, 0) + 1),
                 fetched_at = now()
             RETURNING id, (metrics->>'complaints')::bigint AS complaints",
        )
        .bind(source::FBL)
        .bind(provider)
        .bind(subject_kind::PROVIDER)
        .bind(day)
        .fetch_one(self.pool())
        .await
        .map_err(sqlx_error)?;
        let complaints: i64 = row.get("complaints");
        sqlx::query("UPDATE feedback_snapshots SET detail = $2 WHERE id = $1")
            .bind(row.get::<i64, _>("id"))
            .bind(fbl_detail(complaints))
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?;
        Ok(())
    }

    async fn set_feedback_source_enabled(
        &self,
        source: &str,
        enabled: bool,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO feedback_sources (source, enabled) VALUES ($1, $2)
             ON CONFLICT (source) DO UPDATE SET enabled = EXCLUDED.enabled",
        )
        .bind(source)
        .bind(enabled)
        .execute(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(())
    }

    async fn claim_feedback_fetch(
        &self,
        source: &str,
        holder: &str,
        lease_seconds: i64,
    ) -> Result<bool, StoreError> {
        sqlx::query("INSERT INTO feedback_sources (source) VALUES ($1) ON CONFLICT DO NOTHING")
            .bind(source)
            .execute(self.pool())
            .await
            .map_err(sqlx_error)?;
        let claimed = sqlx::query(
            "UPDATE feedback_sources
             SET lease_holder = $2, lease_until = now() + make_interval(secs => $3::bigint),
                 last_attempt_at = now()
             WHERE source = $1
               AND (next_attempt_at IS NULL OR next_attempt_at <= now())
               AND (lease_until IS NULL OR lease_until < now())",
        )
        .bind(source)
        .bind(holder)
        .bind(lease_seconds)
        .execute(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(claimed.rows_affected() == 1)
    }

    async fn finish_feedback_fetch(
        &self,
        source: &str,
        holder: &str,
        error: Option<&str>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE feedback_sources
             SET lease_holder = NULL, lease_until = NULL, next_attempt_at = $3,
                 last_error = $4,
                 last_success_at = CASE WHEN $4::text IS NULL THEN last_attempt_at
                                        ELSE last_success_at END,
                 consecutive_failures = CASE WHEN $4::text IS NULL THEN 0
                                             ELSE consecutive_failures + 1 END
             WHERE source = $1 AND lease_holder = $2",
        )
        .bind(source)
        .bind(holder)
        .bind(next_attempt_at)
        .bind(error)
        .execute(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(())
    }

    async fn feedback_source_statuses(&self) -> Result<Vec<FeedbackSourceStatus>, StoreError> {
        let rows = sqlx::query("SELECT * FROM feedback_sources")
            .fetch_all(self.pool())
            .await
            .map_err(sqlx_error)?;
        let statuses: Vec<FeedbackSourceStatus> = rows.iter().map(status_from_row).collect();
        Ok(source::ALL
            .iter()
            .filter_map(|name| statuses.iter().find(|s| s.source == *name).cloned())
            .collect())
    }

    async fn latest_provider_signals(
        &self,
        ip: &str,
        provider: &str,
    ) -> Result<Vec<ProviderSignal>, StoreError> {
        let rows = sqlx::query(&format!(
            "{NEWEST}
             WHERE provider = $2
               AND ((subject_kind = 'ip' AND subject = $1)
                    OR subject_kind IN ('domain', 'provider'))
             ORDER BY provider, subject_kind, subject, data_date DESC"
        ))
        .bind(ip)
        .bind(provider)
        .fetch_all(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(
            newest_per_subject(rows.iter().map(snapshot_from_row).collect())
                .into_iter()
                .map(ProviderSignal::from)
                .collect(),
        )
    }

    async fn prune_feedback_snapshots(&self, days: u32) -> Result<u64, StoreError> {
        let done = sqlx::query(
            "DELETE FROM feedback_snapshots
             WHERE data_date < (now() AT TIME ZONE 'UTC')::date - $1::int",
        )
        .bind(days as i32)
        .execute(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(done.rows_affected())
    }

    async fn server_complaint_suppression(
        &self,
        server_id: camelmailer_core::Id,
    ) -> Result<Option<bool>, StoreError> {
        let value: Option<Option<bool>> = sqlx::query_scalar(
            "SELECT suppress_complainants FROM server_feedback_settings WHERE server_id = $1",
        )
        .bind(server_id as i64)
        .fetch_optional(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(value.flatten())
    }

    async fn set_server_complaint_suppression(
        &self,
        server_id: camelmailer_core::Id,
        suppress: Option<bool>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO server_feedback_settings (server_id, suppress_complainants)
             VALUES ($1, $2)
             ON CONFLICT (server_id) DO UPDATE SET suppress_complainants = EXCLUDED.suppress_complainants",
        )
        .bind(server_id as i64)
        .bind(suppress)
        .execute(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(())
    }

    async fn record_feedback_report(
        &self,
        server_id: camelmailer_core::Id,
        message_id: i64,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO feedback_reports (server_id, message_id) VALUES ($1, $2)
             ON CONFLICT DO NOTHING",
        )
        .bind(server_id as i64)
        .bind(message_id)
        .execute(self.pool())
        .await
        .map_err(sqlx_error)?;
        Ok(())
    }
}
