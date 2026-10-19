//! Postal-compatible webhook message fields, shared by every event that
//! describes a stored message (`MessageBounced`, `MessageLoaded`,
//! `MessageLinkClicked`, `MessageComplained`). Keeping the contract in one
//! place stops the shapes drifting between the worker and the tracking
//! endpoints.

use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::Row;

/// The columns [`message_hash_from_row`] reads. Select exactly these.
pub(crate) const MESSAGE_HASH_COLUMNS: &str =
    "id, token, scope, rcpt_to, mail_from, subject, message_id_header, spam_status, tag, created_at";

/// Postal's `message_id` omits the surrounding angle brackets (and anything
/// around them).
pub fn postal_webhook_message_id(value: Option<&str>) -> Option<String> {
    value.map(|value| {
        let after_open = value.rsplit_once('<').map_or(value, |(_, rest)| rest);
        after_open
            .split_once('>')
            .map_or(after_open, |(message_id, _)| message_id)
            .trim()
            .to_string()
    })
}

/// Postal's `subject` is MIME-decoded, capped at 200 characters, and an
/// empty string (never null) when the message has no Subject header.
pub fn postal_webhook_subject(value: Option<&str>) -> String {
    let Some(value) = value else {
        return String::new();
    };
    let raw_header = format!("Subject: {value}");
    let decoded = mailparse::parse_header(raw_header.as_bytes())
        .map(|(header, _)| header.get_value())
        .unwrap_or_else(|_| value.to_string());
    decoded.chars().take(200).collect()
}

/// Unix seconds with millisecond precision, the `timestamp` format of
/// Postal's message hash.
pub fn postal_webhook_timestamp(at: chrono::DateTime<chrono::Utc>) -> f64 {
    at.timestamp_millis() as f64 / 1000.0
}

/// Postal's message hash (`id`, `token`, `direction`, `message_id`, `to`,
/// `from`, `subject`, `timestamp`, `spam_status`, `tag`) from a `messages`
/// row selected with [`MESSAGE_HASH_COLUMNS`].
pub(crate) fn message_hash_from_row(row: &PgRow) -> Value {
    let subject: Option<String> = row.get("subject");
    let message_id_header: Option<String> = row.get("message_id_header");
    let created_at: chrono::DateTime<chrono::Utc> = row.get("created_at");
    json!({
        "id": row.get::<i64, _>("id"),
        "token": row.get::<String, _>("token"),
        "direction": row.get::<String, _>("scope"),
        "message_id": postal_webhook_message_id(message_id_header.as_deref()),
        "to": row.get::<String, _>("rcpt_to"),
        "from": row.get::<String, _>("mail_from"),
        "subject": postal_webhook_subject(subject.as_deref()),
        "timestamp": postal_webhook_timestamp(created_at),
        "spam_status": row.get::<String, _>("spam_status"),
        "tag": row.get::<Option<String>, _>("tag"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postal_webhook_message_ids_drop_angle_brackets_and_surrounding_text() {
        assert_eq!(
            postal_webhook_message_id(Some("prefix <original@example.com> suffix")),
            Some("original@example.com".to_string())
        );
        assert_eq!(postal_webhook_message_id(None), None);
    }

    #[test]
    fn postal_webhook_subjects_are_decoded_capped_and_never_null() {
        assert_eq!(postal_webhook_subject(Some("=?UTF-8?Q?Ol=C3=A1?=")), "Olá");

        let long = "x".repeat(201);
        assert_eq!(postal_webhook_subject(Some(&long)), "x".repeat(200));
        assert_eq!(postal_webhook_subject(None), "");
    }

    #[test]
    fn postal_webhook_timestamps_keep_milliseconds() {
        let at = chrono::DateTime::from_timestamp_millis(1_720_000_000_123).unwrap();
        assert_eq!(postal_webhook_timestamp(at), 1_720_000_000.123);
    }
}
