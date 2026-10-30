//! Organization quota and suspension.
//!
//! An organization may carry a `monthly_send_limit`: how many outgoing
//! messages all of its servers together may store in one calendar month
//! (UTC). `None` is unlimited, the default for every organization, so the
//! feature is inert on an installation that never sets a value. An
//! organization can also be suspended, which stops outgoing mail from every
//! one of its servers.
//!
//! Usage is counted per organization and month in its own table
//! (`organization_send_counters`), incremented in the same transaction that
//! stores the message. It is never pruned: it is the authoritative monthly
//! figure a billing system reads, and one row per organization and month
//! stays small.
//!
//! The gates are the same two as for the per-server limit (HTTP
//! `enqueue_send` and SMTP end of DATA); both enforce the stricter of the
//! organization and the server allowance, see
//! [`crate::SendAllowance::stricter`].

use chrono::{Datelike, NaiveDate};
use serde::Serialize;

/// The operator-controlled sending policy of an organization.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct OrganizationPolicy {
    /// Outgoing messages per calendar month (UTC) across all servers.
    /// `None` is unlimited.
    pub monthly_send_limit: Option<i64>,
    /// While set, every server of the organization refuses outgoing mail.
    pub suspended: bool,
    /// Why the organization was suspended (shown to its members).
    pub suspension_reason: Option<String>,
    /// Data retention overrides for this organization (plan dependent on a
    /// hosted installation). See [`crate::retention`].
    pub retention: crate::RetentionOverrides,
}

/// The usage thresholds, in percent of the monthly limit, that raise an
/// `organization.usage_threshold` lifecycle event (each once per month).
pub const USAGE_THRESHOLDS: [u8; 2] = [80, 100];

/// The thresholds `sent` has reached for `limit`. Empty for an unlimited or
/// zero limit (a zero limit stops every send, so nothing is ever counted).
pub fn reached_thresholds(sent: i64, limit: Option<i64>) -> Vec<u8> {
    match limit {
        Some(limit) if limit > 0 => USAGE_THRESHOLDS
            .iter()
            .copied()
            .filter(|threshold| (sent as i128) * 100 >= (limit as i128) * (*threshold as i128))
            .collect(),
        _ => Vec::new(),
    }
}

/// A calendar month in UTC, the unit of the organization quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UsageMonth {
    pub year: i32,
    /// 1..=12
    pub month: u32,
}

impl UsageMonth {
    /// The current month in UTC.
    pub fn current() -> Self {
        Self::of(chrono::Utc::now().date_naive())
    }

    /// The month containing `date`.
    pub fn of(date: NaiveDate) -> Self {
        Self {
            year: date.year(),
            month: date.month(),
        }
    }

    /// Parse `YYYY-MM`.
    pub fn parse(value: &str) -> Option<Self> {
        let (year, month) = value.split_once('-')?;
        if year.len() != 4 || month.len() != 2 {
            return None;
        }
        let year: i32 = year.parse().ok()?;
        let month: u32 = month.parse().ok()?;
        NaiveDate::from_ymd_opt(year, month, 1)?;
        Some(Self { year, month })
    }

    /// The first day of the month (the key of the counter row).
    pub fn first_day(&self) -> NaiveDate {
        NaiveDate::from_ymd_opt(self.year, self.month, 1).expect("valid month")
    }

    /// The first day of the following month (when the quota resets).
    pub fn next_first_day(&self) -> NaiveDate {
        let (year, month) = if self.month == 12 {
            (self.year + 1, 1)
        } else {
            (self.year, self.month + 1)
        };
        NaiveDate::from_ymd_opt(year, month, 1).expect("valid month")
    }

    /// The `YYYY-MM` label used on the wire.
    pub fn label(&self) -> String {
        format!("{:04}-{:02}", self.year, self.month)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn months_parse_and_print_as_yyyy_mm() {
        let month = UsageMonth::parse("2026-10").unwrap();
        assert_eq!(month.label(), "2026-10");
        assert_eq!(month.first_day().to_string(), "2026-10-01");
        assert_eq!(month.next_first_day().to_string(), "2026-11-01");
        assert_eq!(
            UsageMonth::parse("2026-12")
                .unwrap()
                .next_first_day()
                .to_string(),
            "2027-01-01"
        );
        for bad in ["2026-13", "2026-1", "26-10", "2026/10", "", "abcd-ef"] {
            assert_eq!(UsageMonth::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn thresholds_are_reached_at_80_and_100_percent() {
        assert!(reached_thresholds(79, Some(100)).is_empty());
        assert_eq!(reached_thresholds(80, Some(100)), vec![80]);
        assert_eq!(reached_thresholds(100, Some(100)), vec![80, 100]);
        assert_eq!(reached_thresholds(4, Some(5)), vec![80]);
        assert!(reached_thresholds(1_000, None).is_empty());
        assert!(reached_thresholds(1_000, Some(0)).is_empty());
    }
}
