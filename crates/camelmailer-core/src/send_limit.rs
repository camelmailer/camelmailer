//! Per-server send limits.
//!
//! A server carries an optional `send_limit`: how many outgoing messages it
//! may store in the trailing 30-day window. `None` is unlimited, which is
//! what every server has until an operator sets a value, so the feature is
//! inert on an installation that does not use it.
//!
//! The window is the current UTC day plus the 29 before it, which is the
//! same shape as the 30-day usage figure the dashboard already shows. Usage
//! is counted in daily buckets, so pruning old messages under
//! `message_retention_days` cannot hand a server its quota back early.
//!
//! Enforcement happens at the two points where a caller asks to send, before
//! anything is stored:
//!
//! - the HTTP send path (`enqueue_send`), which every API send funnels
//!   through, including broadcasts and campaigns,
//! - SMTP end of DATA, after a completed idempotent retry can be recognized.
//!
//! Both consult [`SendAllowance`], so the two surfaces cannot disagree about
//! what the limit means.

/// How much of a server's send limit is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendAllowance {
    /// The configured limit, or `None` for unlimited.
    pub limit: Option<i64>,
    /// Outgoing messages already counted in the current window.
    pub used: i64,
}

impl SendAllowance {
    pub fn unlimited() -> Self {
        Self {
            limit: None,
            used: 0,
        }
    }

    /// Messages still available, or `None` when the server is unlimited.
    /// Never negative: a limit lowered below current usage reads as zero
    /// remaining rather than as a debt.
    pub fn remaining(&self) -> Option<i64> {
        self.limit.map(|limit| (limit - self.used).max(0))
    }

    /// Whether `count` more messages fit. A request for zero or fewer
    /// messages always fits, since it stores nothing.
    pub fn allows(&self, count: i64) -> bool {
        match self.remaining() {
            None => true,
            Some(remaining) => count <= 0 || count <= remaining,
        }
    }

    /// The message shown to the caller that ran out. It names the limit and
    /// the window, because "send limit exceeded" on its own leaves someone
    /// guessing whether waiting helps.
    pub fn rejection_message(&self) -> String {
        match self.limit {
            None => "This mail server has no send limit".into(),
            Some(limit) => format!(
                "This mail server has reached its send limit of {limit} messages per 30 days ({} used)",
                self.used
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unlimited_server_allows_any_count() {
        let allowance = SendAllowance::unlimited();
        assert_eq!(allowance.remaining(), None);
        assert!(allowance.allows(1));
        assert!(allowance.allows(1_000_000));
    }

    #[test]
    fn a_limited_server_allows_exactly_the_remainder() {
        let allowance = SendAllowance {
            limit: Some(10),
            used: 7,
        };
        assert_eq!(allowance.remaining(), Some(3));
        assert!(allowance.allows(3));
        assert!(!allowance.allows(4));
    }

    #[test]
    fn usage_past_a_lowered_limit_reads_as_zero_remaining() {
        // An operator can drop a limit below what a server already sent.
        // That stops further sending; it does not create a debt to work off
        // once the window rolls forward.
        let allowance = SendAllowance {
            limit: Some(5),
            used: 9,
        };
        assert_eq!(allowance.remaining(), Some(0));
        assert!(!allowance.allows(1));
        assert!(allowance.allows(0));
    }

    #[test]
    fn a_zero_limit_stops_every_send() {
        let allowance = SendAllowance {
            limit: Some(0),
            used: 0,
        };
        assert_eq!(allowance.remaining(), Some(0));
        assert!(!allowance.allows(1));
    }

    #[test]
    fn the_rejection_message_names_the_limit_and_the_window() {
        let message = SendAllowance {
            limit: Some(5000),
            used: 5000,
        }
        .rejection_message();
        assert!(message.contains("5000 messages per 30 days"));
        assert!(message.contains("5000 used"));
    }
}
