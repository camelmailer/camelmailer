//! Installation lifecycle events.
//!
//! These are installation-wide events (not per mail server, unlike message
//! webhooks) that an operator can forward to an outside system such as a
//! billing or CRM service: an organization was created or deleted, a user
//! registered, an organization reached 80 % or 100 % of its monthly send
//! limit.
//!
//! Events are written to an outbox (`lifecycle_events`) and delivered by the
//! worker to every URL in `lifecycle_webhooks.urls`, with retries. Each
//! request carries the JSON envelope built by [`LifecycleEvent::body`] and a
//! signature over `"{timestamp}.{body}"`, HMAC-SHA256 with
//! `lifecycle_webhooks.secret`, in the header
//! `X-Camelmailer-Signature: t=<unix seconds>,v1=<lowercase hex>`.
//! [`verify_signature`] is the receiving side of that scheme.

use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

pub const ORGANIZATION_CREATED: &str = "organization.created";
pub const ORGANIZATION_DELETED: &str = "organization.deleted";
pub const USER_REGISTERED: &str = "user.registered";
pub const ORGANIZATION_USAGE_THRESHOLD: &str = "organization.usage_threshold";

/// The header carrying `t=<unix>,v1=<hex>`.
pub const SIGNATURE_HEADER: &str = "X-Camelmailer-Signature";
/// The header carrying the event type.
pub const EVENT_HEADER: &str = "X-Camelmailer-Event";
/// The header carrying the event id (dedupe key for receivers).
pub const EVENT_ID_HEADER: &str = "X-Camelmailer-Event-Id";

/// One lifecycle event, ready to be queued.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleEvent {
    /// Unique id; deliveries are at-least-once, so receivers dedupe on it.
    pub id: String,
    /// One of the `*` constants in this module.
    pub event: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Event-specific data.
    pub data: Value,
}

impl LifecycleEvent {
    pub fn new(event: &str, data: Value) -> Self {
        Self {
            id: crate::token::generate_uuid(),
            event: event.to_string(),
            created_at: chrono::Utc::now(),
            data,
        }
    }

    /// The JSON request body, rendered once at enqueue time so retries send
    /// identical bytes.
    pub fn body(&self) -> String {
        json!({
            "id": self.id,
            "event": self.event,
            "created_at": self.created_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "data": self.data,
        })
        .to_string()
    }
}

/// The organization part of an event payload.
pub fn organization_data(organization: &crate::Organization) -> Value {
    json!({
        "id": organization.id,
        "uuid": organization.uuid,
        "permalink": organization.permalink,
        "name": organization.name,
    })
}

/// The user part of an event payload.
pub fn user_data(user: &crate::User) -> Value {
    json!({
        "id": user.id,
        "uuid": user.uuid,
        "email_address": user.email_address,
        "first_name": user.first_name,
        "last_name": user.last_name,
    })
}

/// The `organization.usage_threshold` event for one crossing.
pub fn usage_threshold_event(
    organization: &crate::Organization,
    month: crate::UsageMonth,
    threshold: u8,
    sent: i64,
    limit: i64,
) -> LifecycleEvent {
    LifecycleEvent::new(
        ORGANIZATION_USAGE_THRESHOLD,
        json!({
            "organization": organization_data(organization),
            "month": month.label(),
            "threshold": threshold,
            "sent": sent,
            "limit": limit,
        }),
    )
}

fn mac(secret: &str, timestamp: i64, body: &str) -> Hmac<Sha256> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(body.as_bytes());
    mac
}

/// The value of the signature header for `body` sent at `timestamp`.
pub fn signature_header(secret: &str, timestamp: i64, body: &str) -> String {
    let digest = mac(secret, timestamp, body).finalize().into_bytes();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("t={timestamp},v1={hex}")
}

/// Verify a received signature header against the raw body. Rejects a
/// timestamp further than `tolerance_seconds` from `now` (replay window).
pub fn verify_signature(
    secret: &str,
    header: &str,
    body: &str,
    now: i64,
    tolerance_seconds: i64,
) -> bool {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = value.parse::<i64>().ok(),
            Some(("v1", value)) => signatures.push(value.to_string()),
            _ => {}
        }
    }
    let Some(timestamp) = timestamp else {
        return false;
    };
    if (now - timestamp).abs() > tolerance_seconds {
        return false;
    }
    signatures.iter().any(|signature| {
        let Some(bytes) = decode_hex(signature) else {
            return false;
        };
        mac(secret, timestamp, body).verify_slice(&bytes).is_ok()
    })
}

fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(value.get(index..index + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_hmac_sha256_over_timestamp_dot_body() {
        // Reference value computed independently:
        // printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac secret
        let header = signature_header("secret", 1_700_000_000, r#"{"a":1}"#);
        assert_eq!(
            header,
            "t=1700000000,v1=49f24e537407743fa4a0242bb63b94b9a47ee99cbbe071ccd8a22550ae411686"
        );
    }

    #[test]
    fn verification_accepts_the_right_secret_and_rejects_the_rest() {
        let body = r#"{"id":"x","event":"user.registered"}"#;
        let header = signature_header("s3cret", 1_000, body);
        assert!(verify_signature("s3cret", &header, body, 1_100, 300));
        assert!(!verify_signature("other", &header, body, 1_100, 300));
        assert!(!verify_signature("s3cret", &header, "{}", 1_100, 300));
        // outside the replay window
        assert!(!verify_signature("s3cret", &header, body, 2_000, 300));
        assert!(!verify_signature("s3cret", "garbage", body, 1_000, 300));
    }

    #[test]
    fn the_body_is_a_stable_envelope() {
        let event = LifecycleEvent::new(USER_REGISTERED, json!({ "user": { "id": 1 } }));
        let body: Value = serde_json::from_str(&event.body()).unwrap();
        assert_eq!(body["id"], json!(event.id));
        assert_eq!(body["event"], json!("user.registered"));
        assert_eq!(body["data"]["user"]["id"], json!(1));
        assert!(body["created_at"].as_str().unwrap().ends_with('Z'));
    }
}
