//! Data retention: how long each class of stored data is kept.
//!
//! The installation sets defaults in the `retention` config group; an
//! organization may override the per-tenant classes (nullable columns on
//! `organizations`, written through the admin API). Precedence is simple:
//! an organization override, when set, replaces the installation default
//! for that class, in either direction. There is no per-server retention
//! setting, so nothing else takes part.
//!
//! A period is a number of days; `None` keeps the data forever. In overrides
//! `None` means "inherit" and `Some(0)` means "keep forever".
//!
//! Message content can be dropped before message metadata, never after: a
//! content period longer than the metadata period is capped to it, since
//! deleting the message row deletes its content too.

use serde::Serialize;

/// Effective retention of the per-tenant data classes, in days
/// (`None` = keep forever).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RetentionPeriods {
    /// Raw MIME of stored messages, outgoing and incoming. After this the
    /// message row stays with an empty body.
    pub message_content_days: Option<u32>,
    /// Message rows with their deliveries, opens and clicks (with IP and
    /// user agent), links and tracking tokens.
    pub message_metadata_days: Option<u32>,
    /// The webhook delivery log (`webhook_request_log`).
    pub webhook_log_days: Option<u32>,
    /// Received DMARC aggregate reports and their rows.
    pub dmarc_report_days: Option<u32>,
}

/// Per-organization overrides: `None` inherits the installation default,
/// `Some(0)` keeps forever, `Some(n)` keeps `n` days.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RetentionOverrides {
    pub message_content_days: Option<u32>,
    pub message_metadata_days: Option<u32>,
    pub webhook_log_days: Option<u32>,
    pub dmarc_report_days: Option<u32>,
}

impl RetentionOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

fn days(value: u32) -> Option<u32> {
    (value > 0).then_some(value)
}

impl RetentionPeriods {
    /// Installation defaults from config values where `0` keeps forever.
    pub fn from_days(content: u32, metadata: u32, webhook_log: u32, dmarc: u32) -> Self {
        Self {
            message_content_days: days(content),
            message_metadata_days: days(metadata),
            webhook_log_days: days(webhook_log),
            dmarc_report_days: days(dmarc),
        }
        .normalized()
    }

    /// Apply an organization's overrides (organization wins per class).
    pub fn with_overrides(&self, overrides: &RetentionOverrides) -> Self {
        let pick = |default: Option<u32>, override_: Option<u32>| match override_ {
            None => default,
            Some(value) => days(value),
        };
        Self {
            message_content_days: pick(self.message_content_days, overrides.message_content_days),
            message_metadata_days: pick(
                self.message_metadata_days,
                overrides.message_metadata_days,
            ),
            webhook_log_days: pick(self.webhook_log_days, overrides.webhook_log_days),
            dmarc_report_days: pick(self.dmarc_report_days, overrides.dmarc_report_days),
        }
        .normalized()
    }

    /// Cap content at the metadata period: content cannot outlive its row.
    pub fn normalized(mut self) -> Self {
        if let Some(metadata) = self.message_metadata_days {
            self.message_content_days = Some(
                self.message_content_days
                    .map_or(metadata, |content| content.min(metadata)),
            );
        }
        self
    }
}

/// The cutoff before which data with a period of `days` is expired.
pub fn cutoff(
    now: chrono::DateTime<chrono::Utc>,
    days: Option<u32>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    days.map(|days| now - chrono::Duration::days(days as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_keeps_forever_and_content_is_capped_by_metadata() {
        let periods = RetentionPeriods::from_days(0, 0, 0, 0);
        assert_eq!(periods, RetentionPeriods::default());
        let periods = RetentionPeriods::from_days(0, 365, 30, 0);
        assert_eq!(periods.message_content_days, Some(365));
        assert_eq!(periods.message_metadata_days, Some(365));
        let periods = RetentionPeriods::from_days(30, 365, 0, 0);
        assert_eq!(periods.message_content_days, Some(30));
        let periods = RetentionPeriods::from_days(400, 365, 0, 0);
        assert_eq!(periods.message_content_days, Some(365));
    }

    #[test]
    fn organization_overrides_win_in_both_directions() {
        let installation = RetentionPeriods::from_days(30, 365, 90, 180);
        // inherit
        assert_eq!(
            installation.with_overrides(&RetentionOverrides::default()),
            installation
        );
        // longer, shorter, and keep forever
        let periods = installation.with_overrides(&RetentionOverrides {
            message_content_days: Some(90),
            message_metadata_days: Some(0),
            webhook_log_days: Some(7),
            dmarc_report_days: None,
        });
        assert_eq!(periods.message_content_days, Some(90));
        assert_eq!(periods.message_metadata_days, None);
        assert_eq!(periods.webhook_log_days, Some(7));
        assert_eq!(periods.dmarc_report_days, Some(180));
        // an override of metadata below the content default caps content
        let periods = installation.with_overrides(&RetentionOverrides {
            message_metadata_days: Some(10),
            ..Default::default()
        });
        assert_eq!(periods.message_content_days, Some(10));
    }
}
