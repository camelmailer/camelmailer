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

/// Header added to outbound messages so a returned DSN can
/// be tied to the stored message that produced it.
pub const MESSAGE_TOKEN_HEADER: &str = "X-CamelMailer-MsgID";

/// Postal's equivalent header. Recognising it lets CamelMailer correlate
/// bounces for messages imported from, or originally delivered by, Postal.
pub const POSTAL_MESSAGE_TOKEN_HEADER: &str = "X-Postal-MsgID";

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

/// Whether an inbound message has the machine-readable shape of a delivery
/// status notification rather than an auto-reply or other return-path mail.
///
/// Standards-compliant DSNs use `multipart/report` with
/// `report-type=delivery-status` (or a top-level `message/delivery-status`).
/// The final field check retains compatibility with older, non-MIME reports
/// that carry the RFC 3464 recipient/action/status triplet plus a
/// `Reporting-MTA:` field. Requiring `Reporting-MTA:` alongside the triplet
/// keeps a human forward that merely quotes an old DSN ("your message
/// bounced, see below") from qualifying — genuine per-recipient DSN fields
/// are always accompanied by the reporting agent that produced them.
pub fn is_delivery_status_notification(raw_message: &[u8]) -> bool {
    let content_type = crate::message::header_value(raw_message, "content-type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type_is(&content_type, "message/delivery-status") {
        return true;
    }
    if content_type_is(&content_type, "multipart/report") {
        let declares_delivery_status =
            content_type
                .split(';')
                .skip(1)
                .map(str::trim)
                .any(|parameter| {
                    parameter.strip_prefix("report-type=").is_some_and(|value| {
                        value
                            .trim_matches('"')
                            .eq_ignore_ascii_case("delivery-status")
                    })
                });
        if declares_delivery_status || has_content_type_part(raw_message, "message/delivery-status")
        {
            return true;
        }
    }

    let mut has_recipient = false;
    let mut has_bounce_action = false;
    let mut has_bounce_status = false;
    let mut has_reporting_mta = false;
    for line in raw_message.split(|byte| *byte == b'\n') {
        let line = trim_ascii(line.strip_suffix(b"\r").unwrap_or(line));
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        let name = trim_ascii(&line[..separator]);
        let value = trim_ascii(&line[separator + 1..]);
        if name.eq_ignore_ascii_case(b"Final-Recipient")
            || name.eq_ignore_ascii_case(b"Original-Recipient")
        {
            has_recipient |= !value.is_empty();
        } else if name.eq_ignore_ascii_case(b"Action") {
            has_bounce_action |=
                value.eq_ignore_ascii_case(b"failed") || value.eq_ignore_ascii_case(b"delayed");
        } else if name.eq_ignore_ascii_case(b"Status") {
            let status = String::from_utf8_lossy(value);
            has_bounce_status |= status
                .split_whitespace()
                .next()
                .and_then(enhanced_code_class)
                .is_some();
        } else if name.eq_ignore_ascii_case(b"Reporting-MTA") {
            has_reporting_mta |= !value.is_empty();
        }
    }
    has_recipient && has_bounce_action && has_bounce_status && has_reporting_mta
}

/// Whether a delivery-status notification reports a terminal recipient
/// failure. RFC 3464 also uses delivery-status reports for delayed and
/// successful delivery; only `Action: failed` means the MTA stopped trying.
pub fn is_failed_delivery_status_notification(raw_message: &[u8]) -> bool {
    if !is_delivery_status_notification(raw_message) {
        return false;
    }

    let content_type = crate::message::header_value(raw_message, "content-type")
        .unwrap_or_default()
        .to_ascii_lowercase();
    if content_type_is(&content_type, "message/delivery-status") {
        return has_only_failed_actions(raw_message);
    }

    if content_type_is(&content_type, "multipart/report") {
        let mut in_delivery_status_part = false;
        let mut saw_failed_action = false;
        for line in raw_message.split(|byte| *byte == b'\n') {
            let line = trim_ascii(line.strip_suffix(b"\r").unwrap_or(line));
            if line.starts_with(b"--") {
                in_delivery_status_part = false;
                continue;
            }
            let Some(separator) = line.iter().position(|byte| *byte == b':') else {
                continue;
            };
            let name = trim_ascii(&line[..separator]);
            let value = trim_ascii(&line[separator + 1..]);
            if name.eq_ignore_ascii_case(b"Content-Type") {
                in_delivery_status_part =
                    content_type_is(&String::from_utf8_lossy(value), "message/delivery-status");
            } else if in_delivery_status_part && name.eq_ignore_ascii_case(b"Action") {
                if !value.eq_ignore_ascii_case(b"failed") {
                    return false;
                }
                saw_failed_action = true;
            }
        }
        return saw_failed_action;
    }

    // Legacy non-MIME reports have already passed the recipient, action,
    // status, and Reporting-MTA shape gate above.
    has_only_failed_actions(raw_message)
}

fn has_only_failed_actions(raw_message: &[u8]) -> bool {
    let mut saw_failed_action = false;
    for line in raw_message.split(|byte| *byte == b'\n') {
        let line = trim_ascii(line.strip_suffix(b"\r").unwrap_or(line));
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        if trim_ascii(&line[..separator]).eq_ignore_ascii_case(b"Action") {
            if !trim_ascii(&line[separator + 1..]).eq_ignore_ascii_case(b"failed") {
                return false;
            }
            saw_failed_action = true;
        }
    }
    saw_failed_action
}

fn content_type_is(value: &str, expected: &str) -> bool {
    value
        .split(';')
        .next()
        .is_some_and(|media_type| media_type.trim().eq_ignore_ascii_case(expected))
}

fn has_content_type_part(raw_message: &[u8], expected: &str) -> bool {
    raw_message.split(|byte| *byte == b'\n').any(|line| {
        let line = trim_ascii(line.strip_suffix(b"\r").unwrap_or(line));
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            return false;
        };
        if !trim_ascii(&line[..separator]).eq_ignore_ascii_case(b"Content-Type") {
            return false;
        }
        content_type_is(
            &String::from_utf8_lossy(trim_ascii(&line[separator + 1..])),
            expected,
        )
    })
}

/// Add CamelMailer's correlation header to an outbound message.
///
/// A submitter may supply arbitrary message headers. Remove existing
/// CamelMailer or Postal message-token headers from the top-level
/// header block before adding the trusted value, otherwise one outbound
/// message could make a later DSN mark unrelated messages as bounced.
pub fn add_message_token_header(raw_message: &[u8], token: &str) -> Vec<u8> {
    let mut output = Vec::with_capacity(raw_message.len() + MESSAGE_TOKEN_HEADER.len() + 4);
    output.extend_from_slice(MESSAGE_TOKEN_HEADER.as_bytes());
    output.extend_from_slice(b": ");
    output.extend_from_slice(token.as_bytes());
    output.extend_from_slice(b"\r\n");

    let mut position = 0;
    let mut skipping = false;
    while position < raw_message.len() {
        let line_end = raw_message[position..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(raw_message.len(), |offset| position + offset + 1);
        let line_with_ending = &raw_message[position..line_end];
        let line = line_with_ending
            .strip_suffix(b"\n")
            .unwrap_or(line_with_ending);
        let line = line.strip_suffix(b"\r").unwrap_or(line);

        if line.is_empty() {
            output.extend_from_slice(&raw_message[position..]);
            return output;
        }

        let continuation = matches!(line.first(), Some(b' ' | b'\t'));
        if !continuation {
            skipping = line
                .split(|byte| *byte == b':')
                .next()
                .is_some_and(is_message_token_header);
        }
        if !skipping {
            output.extend_from_slice(line_with_ending);
        }
        position = line_end;
    }

    output
}

/// Find message-token header lines anywhere in a DSN, including an embedded
/// returned message. The scan covers the whole MIME body because returned
/// headers may appear at different nesting levels. A match is harmless unless
/// its generated token resolves to an outgoing message in the same tenant.
/// Values are deduplicated in encounter order.
pub fn original_message_tokens(raw_message: &[u8]) -> Vec<String> {
    let mut tokens = Vec::new();
    for line in raw_message.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(separator) = line.iter().position(|byte| *byte == b':') else {
            continue;
        };
        if !is_message_token_header(&line[..separator]) {
            continue;
        }
        let value = trim_ascii(&line[separator + 1..]);
        let token: Vec<u8> = value
            .iter()
            .copied()
            .take_while(u8::is_ascii_alphanumeric)
            .collect();
        if token.is_empty() || token.len() > 64 {
            continue;
        }
        let token = String::from_utf8(token).expect("ASCII token");
        if !tokens.contains(&token) {
            tokens.push(token);
        }
    }
    tokens
}

fn is_message_token_header(name: &[u8]) -> bool {
    name.eq_ignore_ascii_case(MESSAGE_TOKEN_HEADER.as_bytes())
        || name.eq_ignore_ascii_case(POSTAL_MESSAGE_TOKEN_HEADER.as_bytes())
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
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

    #[test]
    fn delivery_status_shape_excludes_auto_replies_and_feedback_reports() {
        let dsn = b"Content-Type: multipart/report;\r\n\
            \treport-type=delivery-status; boundary=b\r\n\r\n\
            --b\r\nContent-Type: message/delivery-status\r\n\r\n\
            Final-Recipient: rfc822; gone@example.com\r\n\
            Action: failed\r\nStatus: 5.1.1\r\n";
        assert!(is_delivery_status_notification(dsn));

        let legacy = b"Subject: failure\r\n\r\n\
            Reporting-MTA: dns; mx.example.com\r\n\
            Final-Recipient: rfc822; gone@example.com\r\n\
            Action: delayed\r\nStatus: 4.4.1\r\n";
        assert!(is_delivery_status_notification(legacy));

        let auto_reply = b"Content-Type: multipart/mixed; boundary=b\r\n\r\n\
            I am away.\r\nX-CamelMailer-MsgID: quoted123\r\n";
        assert!(!is_delivery_status_notification(auto_reply));

        let feedback = b"Content-Type: multipart/report; report-type=feedback-report\r\n\r\n\
            Feedback-Type: abuse\r\nX-CamelMailer-MsgID: quoted123\r\n";
        assert!(!is_delivery_status_notification(feedback));
    }

    #[test]
    fn only_failed_delivery_status_actions_are_terminal() {
        for action in ["delayed", "delivered", "relayed", "expanded"] {
            let report = format!(
                "Content-Type: multipart/report; report-type=delivery-status; boundary=b\r\n\r\n\
                 --b\r\nContent-Type: message/delivery-status\r\n\r\n\
                 Final-Recipient: rfc822; user@example.com\r\n\
                 Action: {action}\r\nStatus: 4.4.1\r\n--b--\r\n"
            );
            assert!(is_delivery_status_notification(report.as_bytes()));
            assert!(!is_failed_delivery_status_notification(report.as_bytes()));
        }

        let failed = b"Content-Type: message/delivery-status\r\n\r\n\
            Final-Recipient: rfc822; user@example.com\r\n\
            Action: failed\r\nStatus: 4.4.1\r\n";
        assert!(is_failed_delivery_status_notification(failed));
        assert_eq!(classify_dsn(failed), BounceCategory::Soft);

        let mixed = b"Content-Type: message/delivery-status\r\n\r\n\
            Final-Recipient: rfc822; first@example.com\r\n\
            Action: delivered\r\nStatus: 2.0.0\r\n\r\n\
            Final-Recipient: rfc822; second@example.com\r\n\
            Action: failed\r\nStatus: 5.1.1\r\n";
        assert!(!is_failed_delivery_status_notification(mixed));
    }

    #[test]
    fn a_forward_quoting_an_old_dsns_fields_does_not_qualify() {
        // No Reporting-MTA: field, so quoting the triplet from an earlier
        // bounce inside a human forward must not pass the legacy gate.
        let forwarded = b"Subject: Fwd: your message bounced, see below\r\n\r\n\
            Hi, this bounced last week, any idea why?\r\n\r\n\
            Final-Recipient: rfc822; gone@example.com\r\n\
            Action: failed\r\nStatus: 5.1.1\r\n";
        assert!(!is_delivery_status_notification(forwarded));
    }

    #[test]
    fn message_token_header_replaces_untrusted_top_level_values() {
        let raw = b"From: sender@example.com\r\n\
            X-CamelMailer-MsgID: forged-one\r\n\
            \tcontinued-forgery\r\n\
            X-Postal-MsgID: forged-two\r\n\
            Subject: Test\r\n\r\n\
            binary\0body\r\n";

        let identified = add_message_token_header(raw, "trusted123");
        assert!(identified.starts_with(b"X-CamelMailer-MsgID: trusted123\r\nFrom:"));
        assert!(!identified
            .windows(b"forged-one".len())
            .any(|window| window == b"forged-one"));
        assert!(!identified
            .windows(b"forged-two".len())
            .any(|window| window == b"forged-two"));
        assert!(identified.ends_with(b"binary\0body\r\n"));
    }

    #[test]
    fn dsns_find_camelmailer_and_postal_message_tokens() {
        let dsn = b"Content-Type: multipart/report; report-type=delivery-status\r\n\r\n\
            X-CamelMailer-MsgID: first123\r\n\
            x-postal-msgid: second456 trailing text\r\n\
            X-CamelMailer-MsgID: first123\r\n\
            X-CamelMailer-MsgID: punctuation-is-not-a-token!\r\n";

        assert_eq!(
            original_message_tokens(dsn),
            vec!["first123", "second456", "punctuation"]
        );
    }
}
