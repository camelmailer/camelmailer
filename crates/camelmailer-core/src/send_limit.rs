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
//!
//! An organization can carry a second, monthly limit across all of its
//! servers (see [`crate::quota`]). Both gates build one allowance per scope
//! and enforce the stricter of the two ([`SendAllowance::stricter`]), so a
//! per-server limit stays useful as an operator override below the plan.

/// Which limit an allowance describes. It decides the error code and the
/// wording of the rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LimitScope {
    /// `servers.send_limit`, a trailing 30-day window.
    #[default]
    Server,
    /// `organizations.monthly_send_limit`, the current calendar month (UTC)
    /// across every server of the organization.
    Organization,
}

/// How much of a send limit is left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendAllowance {
    /// The configured limit, or `None` for unlimited.
    pub limit: Option<i64>,
    /// Outgoing messages already counted in the current window.
    pub used: i64,
    /// The limit this allowance describes.
    pub scope: LimitScope,
}

impl SendAllowance {
    pub fn unlimited() -> Self {
        Self {
            limit: None,
            used: 0,
            scope: LimitScope::Server,
        }
    }

    /// A server's 30-day allowance.
    pub fn server(limit: Option<i64>, used: i64) -> Self {
        Self {
            limit,
            used,
            scope: LimitScope::Server,
        }
    }

    /// An organization's allowance for the current calendar month.
    pub fn organization(limit: Option<i64>, used: i64) -> Self {
        Self {
            limit,
            used,
            scope: LimitScope::Organization,
        }
    }

    /// The allowance with less room left. Unlimited loses to any limit; on a
    /// tie the organization wins, since its message tells the sender more
    /// (a plan quota resets with the month).
    pub fn stricter(self, other: Self) -> Self {
        match (self.remaining(), other.remaining()) {
            (None, _) => other,
            (_, None) => self,
            (Some(a), Some(b)) if a < b => self,
            (Some(a), Some(b)) if b < a => other,
            _ => {
                if self.scope == LimitScope::Organization {
                    self
                } else {
                    other
                }
            }
        }
    }

    /// The stable API error code for a rejection by this allowance.
    pub fn error_code(&self) -> &'static str {
        match self.scope {
            LimitScope::Server => "SendLimitExceeded",
            LimitScope::Organization => "OrganizationQuotaExceeded",
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
        match (self.scope, self.limit) {
            (LimitScope::Server, None) => "This mail server has no send limit".into(),
            (LimitScope::Server, Some(limit)) => format!(
                "This mail server has reached its send limit of {limit} messages per 30 days ({} used)",
                self.used
            ),
            (LimitScope::Organization, None) => "This organization has no monthly send limit".into(),
            (LimitScope::Organization, Some(limit)) => format!(
                "This organization has reached its monthly send limit of {limit} messages ({} used this month); it resets on the first day of the next month (UTC)",
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
        let allowance = SendAllowance::server(Some(10), 7);
        assert_eq!(allowance.remaining(), Some(3));
        assert!(allowance.allows(3));
        assert!(!allowance.allows(4));
    }

    #[test]
    fn usage_past_a_lowered_limit_reads_as_zero_remaining() {
        // An operator can drop a limit below what a server already sent.
        // That stops further sending; it does not create a debt to work off
        // once the window rolls forward.
        let allowance = SendAllowance::server(Some(5), 9);
        assert_eq!(allowance.remaining(), Some(0));
        assert!(!allowance.allows(1));
        assert!(allowance.allows(0));
    }

    #[test]
    fn a_zero_limit_stops_every_send() {
        let allowance = SendAllowance::server(Some(0), 0);
        assert_eq!(allowance.remaining(), Some(0));
        assert!(!allowance.allows(1));
    }

    #[test]
    fn the_rejection_message_names_the_limit_and_the_window() {
        let message = SendAllowance::server(Some(5000), 5000).rejection_message();
        assert!(message.contains("5000 messages per 30 days"));
        assert!(message.contains("5000 used"));
    }

    #[test]
    fn the_stricter_allowance_is_the_one_with_less_room() {
        let server = SendAllowance::server(Some(100), 10);
        let org = SendAllowance::organization(Some(50), 45);
        assert_eq!(server.stricter(org), org);
        assert_eq!(org.stricter(server), org);
        let roomy_org = SendAllowance::organization(Some(1000), 0);
        assert_eq!(server.stricter(roomy_org), server);
        assert_eq!(
            SendAllowance::unlimited().stricter(org),
            org,
            "unlimited never wins"
        );
        assert_eq!(org.stricter(SendAllowance::unlimited()), org);
    }

    #[test]
    fn the_organization_scope_has_its_own_code_and_message() {
        let org = SendAllowance::organization(Some(5000), 5000);
        assert_eq!(org.error_code(), "OrganizationQuotaExceeded");
        assert!(org
            .rejection_message()
            .contains("monthly send limit of 5000"));
        assert_eq!(
            SendAllowance::server(Some(1), 1).error_code(),
            "SendLimitExceeded"
        );
    }
}
