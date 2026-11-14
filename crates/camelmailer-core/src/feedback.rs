//! Provider feedback: what the large mailbox providers report about this
//! installation's mail, stored as dated snapshots.
//!
//! Three sources feed it, each optional:
//!
//! - **Google Postmaster Tools** (API v2): per sending domain and day, the
//!   user-reported spam rate, SPF/DKIM/DMARC success, outbound TLS and the
//!   delivery error rates. Domain and IP reputation are not part of v2;
//!   Google retired them (https://support.google.com/mail/answer/16594218).
//! - **Microsoft SNDS**: per sending IP and day, the filter result
//!   (green/yellow/red), the complaint rate and trap hits.
//! - **Complaint feedback loops** (Microsoft JMRP, Yahoo CFL, others): ARF
//!   reports counted per provider and day.
//!
//! Each snapshot carries a normalised [`Verdict`], so a consumer such as the
//! warm-up brake can ask [`FeedbackStore::latest_provider_signals`] for one IP
//! and provider and act on `ok` / `warn` / `bad` without knowing the source
//! formats.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::admin_store::StoreError;

/// Source names as stored.
pub mod source {
    pub const POSTMASTER_TOOLS: &str = "postmaster_tools";
    pub const SNDS: &str = "snds";
    pub const FBL: &str = "fbl";
    /// Every source, in display order.
    pub const ALL: [&str; 3] = [POSTMASTER_TOOLS, SNDS, FBL];
}

/// Provider group names, shared with the IP metrics and the warm-up.
pub mod provider {
    pub const GOOGLE: &str = "google";
    pub const MICROSOFT: &str = "microsoft";
    pub const YAHOO: &str = "yahoo";
    pub const OTHER: &str = "other";
}

/// What a snapshot is about.
pub mod subject_kind {
    /// A sending domain (Postmaster Tools).
    pub const DOMAIN: &str = "domain";
    /// A sending IP address (SNDS).
    pub const IP: &str = "ip";
    /// The provider as a whole (feedback-loop counts).
    pub const PROVIDER: &str = "provider";
}

/// A normalised reading of one snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Not enough data to judge (or a count without a volume to relate it
    /// to).
    Unknown,
    /// Healthy.
    Ok,
    /// Worth slowing down for.
    Warn,
    /// Stop and step back.
    Bad,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Unknown => "unknown",
            Verdict::Ok => "ok",
            Verdict::Warn => "warn",
            Verdict::Bad => "bad",
        }
    }

    pub fn parse(value: &str) -> Verdict {
        match value {
            "ok" => Verdict::Ok,
            "warn" => Verdict::Warn,
            "bad" => Verdict::Bad,
            _ => Verdict::Unknown,
        }
    }
}

/// A snapshot to store. The key is (`source`, `provider`, `subject_kind`,
/// `subject`, `data_date`); storing the same key again replaces it.
#[derive(Debug, Clone, PartialEq)]
pub struct NewFeedbackSnapshot {
    pub source: String,
    pub provider: String,
    pub subject_kind: String,
    pub subject: String,
    /// The day the provider's data is about (not the fetch day).
    pub data_date: NaiveDate,
    pub verdict: Verdict,
    /// One line explaining the verdict.
    pub detail: String,
    /// The provider's values, as fetched.
    pub metrics: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackSnapshot {
    pub source: String,
    pub provider: String,
    pub subject_kind: String,
    pub subject: String,
    pub data_date: NaiveDate,
    pub verdict: Verdict,
    pub detail: String,
    pub metrics: Value,
    pub fetched_at: DateTime<Utc>,
}

/// The fetch state of one source.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FeedbackSourceStatus {
    pub source: String,
    /// Configured (credentials present) on the worker that last checked.
    pub enabled: bool,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub consecutive_failures: i32,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

/// One signal for one IP and provider, for the warm-up brake.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProviderSignal {
    pub source: String,
    /// The IP, domain or provider the signal is about.
    pub subject_kind: String,
    pub subject: String,
    pub verdict: Verdict,
    pub detail: String,
    pub data_date: NaiveDate,
    pub observed_at: DateTime<Utc>,
}

/// Storage for provider feedback. Implemented in lockstep by `MemoryStore`
/// and the Postgres store.
#[async_trait]
pub trait FeedbackStore: Send + Sync {
    /// Insert or replace snapshots by their key. Returns how many were
    /// written.
    async fn upsert_feedback_snapshots(
        &self,
        rows: &[NewFeedbackSnapshot],
    ) -> Result<usize, StoreError>;

    /// The newest snapshot of every (provider, subject) of `source`,
    /// worst verdict first, then by subject.
    async fn latest_feedback_snapshots(
        &self,
        source: &str,
    ) -> Result<Vec<FeedbackSnapshot>, StoreError>;

    /// Count one feedback-loop complaint reported by `provider` on `day`.
    async fn record_fbl_complaint(&self, provider: &str, day: NaiveDate) -> Result<(), StoreError>;

    /// Record whether `source` is configured on this worker. Creates the
    /// status row when it is missing.
    async fn set_feedback_source_enabled(
        &self,
        source: &str,
        enabled: bool,
    ) -> Result<(), StoreError>;

    /// Claim the next fetch of `source` for `holder` when it is due and no
    /// other worker holds it, for at most `lease_seconds`. Only one worker
    /// fetches a source at a time.
    async fn claim_feedback_fetch(
        &self,
        source: &str,
        holder: &str,
        lease_seconds: i64,
    ) -> Result<bool, StoreError>;

    /// Record the result of a claimed fetch and when the next one is due;
    /// releases the claim.
    async fn finish_feedback_fetch(
        &self,
        source: &str,
        holder: &str,
        error: Option<&str>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError>;

    /// Every source's fetch state, in [`source::ALL`] order; sources never
    /// seen are missing.
    async fn feedback_source_statuses(&self) -> Result<Vec<FeedbackSourceStatus>, StoreError>;

    /// The newest signal of every source about `ip` for `provider`: SNDS
    /// rows for that IP, and the domain-wide (Postmaster Tools) and
    /// provider-wide (feedback loop) rows of that provider, which apply to
    /// every IP. Worst verdict first.
    async fn latest_provider_signals(
        &self,
        ip: &str,
        provider: &str,
    ) -> Result<Vec<ProviderSignal>, StoreError>;

    /// Delete snapshots whose data is older than `days` days. Returns how
    /// many were deleted.
    async fn prune_feedback_snapshots(&self, days: u32) -> Result<u64, StoreError>;

    // -- complaint handling per mail server

    /// Whether `server_id` suppresses complainants of transactional mail;
    /// `None` follows the installation default.
    async fn server_complaint_suppression(
        &self,
        server_id: crate::model::Id,
    ) -> Result<Option<bool>, StoreError>;

    /// Set (or with `None` clear) the server's own choice.
    async fn set_server_complaint_suppression(
        &self,
        server_id: crate::model::Id,
        suppress: Option<bool>,
    ) -> Result<(), StoreError>;

    /// Remember that an inbound message was an ARF report, so retention can
    /// remove its raw content after `retention.feedback_report_days`.
    async fn record_feedback_report(
        &self,
        server_id: crate::model::Id,
        message_id: i64,
    ) -> Result<(), StoreError>;
}

// ------------------------------------------------------------- helpers

/// Keep the newest snapshot per (provider, subject_kind, subject) and order
/// worst verdict first. Shared by both stores so they agree.
pub fn newest_per_subject(mut rows: Vec<FeedbackSnapshot>) -> Vec<FeedbackSnapshot> {
    rows.sort_by(|a, b| {
        (&a.provider, &a.subject_kind, &a.subject)
            .cmp(&(&b.provider, &b.subject_kind, &b.subject))
            .then(b.data_date.cmp(&a.data_date))
    });
    rows.dedup_by(|later, first| {
        later.provider == first.provider
            && later.subject_kind == first.subject_kind
            && later.subject == first.subject
    });
    rows.sort_by(|a, b| {
        b.verdict
            .cmp(&a.verdict)
            .then(a.provider.cmp(&b.provider))
            .then(a.subject.cmp(&b.subject))
    });
    rows
}

/// Whether a snapshot is a signal for `ip` at `provider`.
pub fn applies_to(snapshot: &FeedbackSnapshot, ip: &str, provider: &str) -> bool {
    snapshot.provider == provider
        && match snapshot.subject_kind.as_str() {
            subject_kind::IP => snapshot.subject == ip,
            subject_kind::DOMAIN | subject_kind::PROVIDER => true,
            _ => false,
        }
}

impl From<FeedbackSnapshot> for ProviderSignal {
    fn from(snapshot: FeedbackSnapshot) -> Self {
        ProviderSignal {
            source: snapshot.source,
            subject_kind: snapshot.subject_kind,
            subject: snapshot.subject,
            verdict: snapshot.verdict,
            detail: snapshot.detail,
            data_date: snapshot.data_date,
            observed_at: snapshot.fetched_at,
        }
    }
}

// ------------------------------------------------------------- MemoryStore

use crate::store::MemoryStore;

/// A fetch claim: the holder and until when it holds.
type Lease = Option<(String, DateTime<Utc>)>;

/// The in-memory feedback tables.
#[derive(Debug, Default)]
pub struct MemoryFeedback {
    snapshots: Vec<FeedbackSnapshot>,
    statuses: Vec<(FeedbackSourceStatus, Lease)>,
    server_settings: std::collections::HashMap<crate::model::Id, bool>,
    /// (server, message, received at)
    reports: Vec<(crate::model::Id, i64, DateTime<Utc>)>,
}

impl MemoryFeedback {
    fn status_mut(&mut self, source: &str) -> &mut (FeedbackSourceStatus, Lease) {
        if let Some(index) = self.statuses.iter().position(|(s, _)| s.source == source) {
            return &mut self.statuses[index];
        }
        self.statuses.push((
            FeedbackSourceStatus {
                source: source.to_string(),
                enabled: false,
                last_attempt_at: None,
                last_success_at: None,
                last_error: None,
                consecutive_failures: 0,
                next_attempt_at: None,
            },
            None,
        ));
        self.statuses.last_mut().expect("just pushed")
    }
}

#[async_trait]
impl FeedbackStore for MemoryStore {
    async fn upsert_feedback_snapshots(
        &self,
        rows: &[NewFeedbackSnapshot],
    ) -> Result<usize, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let now = Utc::now();
        for row in rows {
            let snapshots = &mut inner.feedback.snapshots;
            snapshots.retain(|s| {
                !(s.source == row.source
                    && s.provider == row.provider
                    && s.subject_kind == row.subject_kind
                    && s.subject == row.subject
                    && s.data_date == row.data_date)
            });
            snapshots.push(FeedbackSnapshot {
                source: row.source.clone(),
                provider: row.provider.clone(),
                subject_kind: row.subject_kind.clone(),
                subject: row.subject.clone(),
                data_date: row.data_date,
                verdict: row.verdict,
                detail: row.detail.clone(),
                metrics: row.metrics.clone(),
                fetched_at: now,
            });
        }
        Ok(rows.len())
    }

    async fn latest_feedback_snapshots(
        &self,
        source: &str,
    ) -> Result<Vec<FeedbackSnapshot>, StoreError> {
        let inner = self.inner.read().unwrap();
        let rows = inner
            .feedback
            .snapshots
            .iter()
            .filter(|s| s.source == source)
            .cloned()
            .collect();
        Ok(newest_per_subject(rows))
    }

    async fn record_fbl_complaint(&self, provider: &str, day: NaiveDate) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let now = Utc::now();
        let snapshots = &mut inner.feedback.snapshots;
        if let Some(existing) = snapshots.iter_mut().find(|s| {
            s.source == source::FBL
                && s.provider == provider
                && s.subject_kind == subject_kind::PROVIDER
                && s.subject == provider
                && s.data_date == day
        }) {
            let count = existing.metrics["complaints"].as_i64().unwrap_or(0) + 1;
            existing.metrics = serde_json::json!({ "complaints": count });
            existing.detail = fbl_detail(count);
            existing.fetched_at = now;
        } else {
            snapshots.push(FeedbackSnapshot {
                source: source::FBL.into(),
                provider: provider.into(),
                subject_kind: subject_kind::PROVIDER.into(),
                subject: provider.into(),
                data_date: day,
                verdict: Verdict::Unknown,
                detail: fbl_detail(1),
                metrics: serde_json::json!({ "complaints": 1 }),
                fetched_at: now,
            });
        }
        Ok(())
    }

    async fn set_feedback_source_enabled(
        &self,
        source: &str,
        enabled: bool,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        inner.feedback.status_mut(source).0.enabled = enabled;
        Ok(())
    }

    async fn claim_feedback_fetch(
        &self,
        source: &str,
        holder: &str,
        lease_seconds: i64,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let now = Utc::now();
        let (status, lease) = inner.feedback.status_mut(source);
        let due = status.next_attempt_at.is_none_or(|at| at <= now);
        let free = lease.as_ref().is_none_or(|(_, until)| *until < now);
        if !(due && free) {
            return Ok(false);
        }
        *lease = Some((
            holder.to_string(),
            now + chrono::Duration::seconds(lease_seconds),
        ));
        status.last_attempt_at = Some(now);
        Ok(true)
    }

    async fn finish_feedback_fetch(
        &self,
        source: &str,
        holder: &str,
        error: Option<&str>,
        next_attempt_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let (status, lease) = inner.feedback.status_mut(source);
        if lease.as_ref().map(|(h, _)| h.as_str()) != Some(holder) {
            return Ok(());
        }
        *lease = None;
        status.next_attempt_at = Some(next_attempt_at);
        match error {
            None => {
                status.last_success_at = status.last_attempt_at;
                status.last_error = None;
                status.consecutive_failures = 0;
            }
            Some(error) => {
                status.last_error = Some(error.to_string());
                status.consecutive_failures += 1;
            }
        }
        Ok(())
    }

    async fn feedback_source_statuses(&self) -> Result<Vec<FeedbackSourceStatus>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(source::ALL
            .iter()
            .filter_map(|name| {
                inner
                    .feedback
                    .statuses
                    .iter()
                    .find(|(s, _)| s.source == *name)
                    .map(|(s, _)| s.clone())
            })
            .collect())
    }

    async fn latest_provider_signals(
        &self,
        ip: &str,
        provider: &str,
    ) -> Result<Vec<ProviderSignal>, StoreError> {
        let inner = self.inner.read().unwrap();
        let rows = inner
            .feedback
            .snapshots
            .iter()
            .filter(|s| applies_to(s, ip, provider))
            .cloned()
            .collect();
        Ok(newest_per_subject(rows)
            .into_iter()
            .map(ProviderSignal::from)
            .collect())
    }

    async fn prune_feedback_snapshots(&self, days: u32) -> Result<u64, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let cutoff = Utc::now().date_naive() - chrono::Duration::days(i64::from(days));
        let before = inner.feedback.snapshots.len();
        inner.feedback.snapshots.retain(|s| s.data_date >= cutoff);
        Ok((before - inner.feedback.snapshots.len()) as u64)
    }

    async fn server_complaint_suppression(
        &self,
        server_id: crate::model::Id,
    ) -> Result<Option<bool>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .feedback
            .server_settings
            .get(&server_id)
            .copied())
    }

    async fn set_server_complaint_suppression(
        &self,
        server_id: crate::model::Id,
        suppress: Option<bool>,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        match suppress {
            Some(value) => {
                inner.feedback.server_settings.insert(server_id, value);
            }
            None => {
                inner.feedback.server_settings.remove(&server_id);
            }
        }
        Ok(())
    }

    async fn record_feedback_report(
        &self,
        server_id: crate::model::Id,
        message_id: i64,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let reports = &mut inner.feedback.reports;
        if !reports
            .iter()
            .any(|(s, m, _)| *s == server_id && *m == message_id)
        {
            reports.push((server_id, message_id, Utc::now()));
        }
        Ok(())
    }
}

/// The detail line of a feedback-loop count.
pub fn fbl_detail(complaints: i64) -> String {
    if complaints == 1 {
        "1 complaint reported".to_string()
    } else {
        format!("{complaints} complaints reported")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(
        source: &str,
        provider: &str,
        kind: &str,
        subject: &str,
        day: u32,
        verdict: Verdict,
    ) -> NewFeedbackSnapshot {
        NewFeedbackSnapshot {
            source: source.into(),
            provider: provider.into(),
            subject_kind: kind.into(),
            subject: subject.into(),
            data_date: NaiveDate::from_ymd_opt(2026, 11, day).unwrap(),
            verdict,
            detail: format!("{verdict:?}"),
            metrics: serde_json::json!({}),
        }
    }

    #[tokio::test]
    async fn the_newest_snapshot_per_subject_wins_and_the_worst_comes_first() {
        let store = MemoryStore::new();
        store
            .upsert_feedback_snapshots(&[
                snapshot(
                    source::SNDS,
                    provider::MICROSOFT,
                    subject_kind::IP,
                    "192.0.2.1",
                    10,
                    Verdict::Bad,
                ),
                snapshot(
                    source::SNDS,
                    provider::MICROSOFT,
                    subject_kind::IP,
                    "192.0.2.1",
                    11,
                    Verdict::Ok,
                ),
                snapshot(
                    source::SNDS,
                    provider::MICROSOFT,
                    subject_kind::IP,
                    "192.0.2.2",
                    11,
                    Verdict::Warn,
                ),
            ])
            .await
            .unwrap();
        // replacing a key keeps one row
        store
            .upsert_feedback_snapshots(&[snapshot(
                source::SNDS,
                provider::MICROSOFT,
                subject_kind::IP,
                "192.0.2.2",
                11,
                Verdict::Bad,
            )])
            .await
            .unwrap();
        let latest = store.latest_feedback_snapshots(source::SNDS).await.unwrap();
        let summary: Vec<(&str, Verdict)> = latest
            .iter()
            .map(|s| (s.subject.as_str(), s.verdict))
            .collect();
        assert_eq!(
            summary,
            vec![("192.0.2.2", Verdict::Bad), ("192.0.2.1", Verdict::Ok)]
        );
    }

    #[tokio::test]
    async fn signals_for_an_ip_include_domain_and_provider_wide_rows_of_that_provider() {
        let store = MemoryStore::new();
        store
            .upsert_feedback_snapshots(&[
                snapshot(
                    source::SNDS,
                    provider::MICROSOFT,
                    subject_kind::IP,
                    "192.0.2.1",
                    11,
                    Verdict::Warn,
                ),
                snapshot(
                    source::SNDS,
                    provider::MICROSOFT,
                    subject_kind::IP,
                    "192.0.2.9",
                    11,
                    Verdict::Bad,
                ),
                snapshot(
                    source::POSTMASTER_TOOLS,
                    provider::GOOGLE,
                    subject_kind::DOMAIN,
                    "example.com",
                    11,
                    Verdict::Bad,
                ),
            ])
            .await
            .unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 12).unwrap();
        store
            .record_fbl_complaint(provider::MICROSOFT, day)
            .await
            .unwrap();
        store
            .record_fbl_complaint(provider::MICROSOFT, day)
            .await
            .unwrap();

        let microsoft = store
            .latest_provider_signals("192.0.2.1", provider::MICROSOFT)
            .await
            .unwrap();
        let sources: Vec<(&str, &str)> = microsoft
            .iter()
            .map(|s| (s.source.as_str(), s.subject.as_str()))
            .collect();
        assert_eq!(sources, vec![("snds", "192.0.2.1"), ("fbl", "microsoft")]);
        assert_eq!(microsoft[1].detail, "2 complaints reported");

        let google = store
            .latest_provider_signals("192.0.2.1", provider::GOOGLE)
            .await
            .unwrap();
        assert_eq!(google.len(), 1);
        assert_eq!(google[0].verdict, Verdict::Bad);
    }

    #[tokio::test]
    async fn a_server_choice_overrides_the_default_until_cleared() {
        let store = MemoryStore::new();
        assert_eq!(store.server_complaint_suppression(7).await.unwrap(), None);
        store
            .set_server_complaint_suppression(7, Some(false))
            .await
            .unwrap();
        assert_eq!(
            store.server_complaint_suppression(7).await.unwrap(),
            Some(false)
        );
        store
            .set_server_complaint_suppression(7, None)
            .await
            .unwrap();
        assert_eq!(store.server_complaint_suppression(7).await.unwrap(), None);
        store.record_feedback_report(7, 42).await.unwrap();
        store.record_feedback_report(7, 42).await.unwrap();
        assert_eq!(store.inner.read().unwrap().feedback.reports.len(), 1);
    }

    #[tokio::test]
    async fn one_worker_holds_a_fetch_and_failures_are_counted() {
        let store = MemoryStore::new();
        assert!(store
            .claim_feedback_fetch(source::SNDS, "w1", 60)
            .await
            .unwrap());
        assert!(!store
            .claim_feedback_fetch(source::SNDS, "w2", 60)
            .await
            .unwrap());
        let later = Utc::now() + chrono::Duration::hours(1);
        // a foreign holder cannot finish it
        store
            .finish_feedback_fetch(source::SNDS, "w2", None, later)
            .await
            .unwrap();
        store
            .finish_feedback_fetch(source::SNDS, "w1", Some("HTTP 500"), later)
            .await
            .unwrap();
        let status = &store.feedback_source_statuses().await.unwrap()[0];
        assert_eq!(status.last_error.as_deref(), Some("HTTP 500"));
        assert_eq!(status.consecutive_failures, 1);
        assert!(status.last_success_at.is_none());
        // not due yet
        assert!(!store
            .claim_feedback_fetch(source::SNDS, "w2", 60)
            .await
            .unwrap());
    }
}
