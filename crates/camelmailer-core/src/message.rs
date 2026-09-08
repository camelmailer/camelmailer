//! Message intake — the write-side of `Postal::MessageDB::Message` that the
//! SMTP server needs. The full per-server message database is a later phase;
//! this defines the message record produced by an accepted SMTP transaction
//! and the sink it is queued into.

use crate::model::Id;
use std::collections::HashMap;
use std::sync::Mutex;

/// Parse the header block of a raw message (up to the first empty line),
/// unfolding continuation lines — the slice of `Postal::MessageParser`
/// needed for indexing. Returns lowercased keys.
pub fn parse_headers(raw_message: &[u8]) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in raw_message.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            break;
        }
        let text = String::from_utf8_lossy(line);
        if text.starts_with([' ', '\t']) {
            // folded continuation of the previous header
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(text.trim());
            }
        } else if let Some((key, value)) = text.split_once(':') {
            headers.push((key.trim().to_lowercase(), value.trim().to_string()));
        }
    }
    headers
}

/// Extract one header value (first occurrence, case-insensitive).
pub fn header_value(raw_message: &[u8], name: &str) -> Option<String> {
    let name = name.to_lowercase();
    parse_headers(raw_message)
        .into_iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageScope {
    Incoming,
    Outgoing,
}

impl MessageScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Incoming => "incoming",
            Self::Outgoing => "outgoing",
        }
    }
}

/// A message accepted by the SMTP server, ready to be written to the
/// receiving server's message database and queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuedMessage {
    pub server_id: Id,
    pub rcpt_to: String,
    pub mail_from: String,
    pub raw_message: Vec<u8>,
    pub received_with_ssl: bool,
    pub scope: MessageScope,
    pub bounce: bool,
    pub domain_id: Option<Id>,
    pub credential_id: Option<Id>,
    pub route_id: Option<Id>,
    /// Optional categorization tag (HTTP send / template send).
    pub tag: Option<String>,
    /// Optional per-message metadata (HTTP send).
    pub metadata: Option<serde_json::Value>,
    /// The message stream this message belongs to (HTTP send).
    pub stream_id: Option<Id>,
}

/// The public identity of a message accepted via the HTTP send API.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SentMessage {
    pub id: i64,
    pub token: String,
    pub rcpt_to: String,
}

/// The read model of a stored message, exposed by the per-server read API.
/// Mirrors the `messages` table columns the API surfaces.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRecord {
    pub id: i64,
    pub token: String,
    pub server_id: Id,
    pub scope: String,
    pub rcpt_to: String,
    pub mail_from: String,
    pub subject: Option<String>,
    pub message_id_header: Option<String>,
    pub tag: Option<String>,
    pub status: String,
    pub bounce: bool,
    /// Original outgoing message for an inbound DSN, once correlated.
    pub bounce_for_id: Option<i64>,
    /// Durable correlation marker. Unlike `bounce_for_id`, this survives
    /// retention pruning the original and keeps statistics de-duplicated.
    pub bounce_correlated_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `hard` / `soft` / `undetermined` once the message terminally failed
    /// or was processed as a bounce (see [`crate::bounce`]); `None` until
    /// then.
    pub bounce_category: Option<String>,
    pub spam_status: String,
    pub spam_score: f64,
    pub held: bool,
    pub threat: bool,
    pub size: i64,
    pub metadata: Option<serde_json::Value>,
    pub stream_id: Option<Id>,
    /// The broadcast campaign that produced this message (`messages.campaign_id`).
    pub campaign_id: Option<Id>,
    /// Incoming message re-queued with block rules bypassed.
    pub bypassed: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub raw_message: Vec<u8>,
}

/// Where accepted messages go. The production implementation writes to the
/// PostgreSQL message database; [`MemorySink`] collects them for
/// tests.
pub trait MessageSink: Send + Sync {
    fn queue_message(&self, message: QueuedMessage);

    /// Replay a completed claim before applying the current allowance to a
    /// new submission. A quota rejection must not reserve the key.
    fn queue_messages(
        &self,
        messages: Vec<QueuedMessage>,
        idempotency: Option<crate::server_store::IdempotencyRequest>,
        allowance: crate::SendAllowance,
    ) -> QueueMessagesOutcome;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueMessagesOutcome {
    Stored,
    Replayed,
    Conflict,
    LimitExceeded(String),
    Failed(String),
}

#[derive(Default)]
struct MemorySinkInner {
    messages: Vec<QueuedMessage>,
    idempotency_requests: HashMap<(Id, String), (String, String, chrono::DateTime<chrono::Utc>)>,
}

#[derive(Default)]
pub struct MemorySink {
    inner: Mutex<MemorySinkInner>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn messages(&self) -> Vec<QueuedMessage> {
        self.inner.lock().unwrap().messages.clone()
    }
}

impl MessageSink for MemorySink {
    fn queue_message(&self, message: QueuedMessage) {
        self.inner.lock().unwrap().messages.push(message);
    }

    fn queue_messages(
        &self,
        messages: Vec<QueuedMessage>,
        idempotency: Option<crate::server_store::IdempotencyRequest>,
        allowance: crate::SendAllowance,
    ) -> QueueMessagesOutcome {
        let mut inner = self.inner.lock().unwrap();
        if let Some(request) = &idempotency {
            let cache_key = (
                messages.first().map(|m| m.server_id).unwrap_or(0),
                request.key_hash.clone(),
            );
            if let Some((request_hash, operation, expires_at)) =
                inner.idempotency_requests.get(&cache_key)
            {
                if *expires_at > chrono::Utc::now() {
                    if request_hash == &request.request_hash && operation == &request.operation {
                        return QueueMessagesOutcome::Replayed;
                    }
                    return QueueMessagesOutcome::Conflict;
                }
            }
        }
        if !allowance.allows(
            messages
                .iter()
                .filter(|m| m.scope == MessageScope::Outgoing)
                .count() as i64,
        ) {
            return QueueMessagesOutcome::LimitExceeded(allowance.rejection_message());
        }
        if let Some(request) = &idempotency {
            let cache_key = (
                messages.first().map(|m| m.server_id).unwrap_or(0),
                request.key_hash.clone(),
            );
            inner.idempotency_requests.remove(&cache_key);
            inner.idempotency_requests.insert(
                cache_key,
                (
                    request.request_hash.clone(),
                    request.operation.clone(),
                    request.expires_at,
                ),
            );
        }
        inner.messages.extend(messages);
        QueueMessagesOutcome::Stored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &[u8] = b"Subject: Hello\r\n\
        Message-ID: <abc@example.com>\r\n\
        X-Folded: first part\r\n\
        \tsecond part\r\n\
        \r\n\
        Subject: this is body, not a header\r\n";

    #[test]
    fn parses_headers_until_the_blank_line() {
        let headers = parse_headers(RAW);
        assert_eq!(headers.len(), 3);
        assert_eq!(header_value(RAW, "Subject").as_deref(), Some("Hello"));
        assert_eq!(
            header_value(RAW, "message-id").as_deref(),
            Some("<abc@example.com>")
        );
    }

    #[test]
    fn unfolds_continuation_lines() {
        assert_eq!(
            header_value(RAW, "X-Folded").as_deref(),
            Some("first part second part")
        );
    }

    #[test]
    fn missing_headers_are_none() {
        assert_eq!(header_value(RAW, "From"), None);
    }
}
