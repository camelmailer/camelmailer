//! Bounce classification — mapping SMTP failure responses (and DSN bounce
//! messages) onto the three stable categories the observability API
//! exposes: `hard`, `soft`, `undetermined`.
//!
//! The heuristic is deliberately simple and documented:
//! - a 5xx SMTP reply code (or enhanced status `5.x.x`) → **hard**
//!   (permanent failure — the address will not start working by retrying),
//! - a 4xx reply code (or enhanced status `4.x.x`) → **soft**
//!   (transient failure — greylisting, full mailbox, throttling),
//! - anything else (connection errors, timeouts, unparsable output)
//!   → **undetermined**.
//!
//! The classification is persisted on the message (`bounce_category`) only
//! for *terminal* failures and processed bounce messages, so a retried
//! message that eventually delivers never carries a stale category.

/// The bounce category of a terminally failed or bounced message.
/// String values are stable API vocabulary — never rename them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BounceCategory {
    Hard,
    Soft,
    Undetermined,
}

impl BounceCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Hard => "hard",
            Self::Soft => "soft",
            Self::Undetermined => "undetermined",
        }
    }
}

impl std::fmt::Display for BounceCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Is this token a standalone SMTP reply code (400–599)?
fn reply_code_class(token: &str) -> Option<BounceCategory> {
    // "550", possibly glued to a dash in multiline replies ("550-5.1.1")
    let digits: &str = token.split('-').next().unwrap_or(token);
    if digits.len() == 3 && digits.bytes().all(|b| b.is_ascii_digit()) {
        return match digits.as_bytes()[0] {
            b'5' => Some(BounceCategory::Hard),
            b'4' => Some(BounceCategory::Soft),
            _ => None,
        };
    }
    None
}

/// Is this token an RFC 3463 enhanced status code (`5.1.1`, `4.7.0`, …)?
fn enhanced_code_class(token: &str) -> Option<BounceCategory> {
    let mut parts = token.trim_end_matches(['.', ',', ';', ')']).split('.');
    let class = parts.next()?;
    let subject = parts.next()?;
    let detail = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let numeric = |s: &str| !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit());
    if !numeric(subject) || !numeric(detail) {
        return None;
    }
    match class {
        "5" => Some(BounceCategory::Hard),
        "4" => Some(BounceCategory::Soft),
        _ => None,
    }
}

/// Classify one SMTP failure response (what the remote server said, as
/// recorded in the delivery's `output`). 5xx → hard, 4xx → soft,
/// otherwise undetermined.
pub fn classify_response(response: &str) -> BounceCategory {
    for token in response.split_whitespace() {
        if let Some(category) = reply_code_class(token) {
            return category;
        }
        if let Some(category) = enhanced_code_class(token) {
            return category;
        }
    }
    BounceCategory::Undetermined
}

/// Classify an inbound bounce (DSN) message from its raw content. Only the
/// diagnostic fields of the delivery-status part are considered
/// (`Status:` and `Diagnostic-Code:` lines), so arbitrary numbers in the
/// human-readable text cannot misclassify — a DSN without those fields is
/// `undetermined`.
pub fn classify_dsn(raw_message: &[u8]) -> BounceCategory {
    let text = String::from_utf8_lossy(raw_message);
    for line in text.lines() {
        let trimmed = line.trim();
        let value = ["Status:", "Diagnostic-Code:"].iter().find_map(|prefix| {
            (trimmed.len() >= prefix.len() && trimmed[..prefix.len()].eq_ignore_ascii_case(prefix))
                .then(|| &trimmed[prefix.len()..])
        });
        if let Some(value) = value {
            let category = classify_response(value);
            if category != BounceCategory::Undetermined {
                return category;
            }
        }
    }
    BounceCategory::Undetermined
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_xx_replies_are_hard() {
        assert_eq!(
            classify_response("550 5.1.1 The email account does not exist"),
            BounceCategory::Hard
        );
        assert_eq!(
            classify_response("554 Transaction failed"),
            BounceCategory::Hard
        );
        assert_eq!(
            classify_response("550-5.1.1 multiline reject"),
            BounceCategory::Hard
        );
    }

    #[test]
    fn four_xx_replies_are_soft() {
        assert_eq!(
            classify_response("421 Service not available, try later"),
            BounceCategory::Soft
        );
        assert_eq!(
            classify_response("452 4.2.2 Mailbox full"),
            BounceCategory::Soft
        );
    }

    #[test]
    fn enhanced_status_codes_classify_without_a_reply_code() {
        assert_eq!(
            classify_response("smtp; 5.7.1 blocked"),
            BounceCategory::Hard
        );
        assert_eq!(classify_response("4.7.0 greylisted."), BounceCategory::Soft);
    }

    #[test]
    fn unparsable_output_is_undetermined() {
        assert_eq!(
            classify_response("connection timed out"),
            BounceCategory::Undetermined
        );
        assert_eq!(classify_response(""), BounceCategory::Undetermined);
        // 3-digit numbers outside 4xx/5xx and years do not classify
        assert_eq!(
            classify_response("released in 2026 on port 251"),
            BounceCategory::Undetermined
        );
    }

    #[test]
    fn dsn_status_fields_classify_bounce_messages() {
        let hard = b"Subject: Delivery Status Notification\r\n\r\n\
            Final-Recipient: rfc822; gone@example.com\r\n\
            Action: failed\r\n\
            Status: 5.1.1\r\n\
            Diagnostic-Code: smtp; 550 5.1.1 user unknown\r\n";
        assert_eq!(classify_dsn(hard), BounceCategory::Hard);

        let soft = b"Subject: Delayed\r\n\r\nstatus: 4.4.1\r\n";
        assert_eq!(classify_dsn(soft), BounceCategory::Soft);

        // numbers in free text never classify
        let noise = b"Subject: bounce\r\n\r\nYour mail from 2026 got 550 problems.\r\n";
        assert_eq!(classify_dsn(noise), BounceCategory::Undetermined);
    }
}
