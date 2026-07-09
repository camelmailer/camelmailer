//! Message intake — the write-side of `Postal::MessageDB::Message` that the
//! SMTP server needs. The full per-server message database is a later phase;
//! this defines the message record produced by an accepted SMTP transaction
//! and the sink it is queued into.

use crate::model::Id;
use std::sync::Mutex;

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
}

/// Where accepted messages go. The production implementation writes to the
/// per-server MariaDB message database; [`MemorySink`] collects them for
/// tests.
pub trait MessageSink: Send + Sync {
    fn queue_message(&self, message: QueuedMessage);
}

#[derive(Default)]
pub struct MemorySink {
    messages: Mutex<Vec<QueuedMessage>>,
}

impl MemorySink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn messages(&self) -> Vec<QueuedMessage> {
        self.messages.lock().unwrap().clone()
    }
}

impl MessageSink for MemorySink {
    fn queue_message(&self, message: QueuedMessage) {
        self.messages.lock().unwrap().push(message);
    }
}
