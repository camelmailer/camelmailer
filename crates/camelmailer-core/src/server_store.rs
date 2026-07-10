//! The tenant-scoped storage interface behind the per-server API
//! (`/api/v2/server/...`). Mirrors the `AdminStore`/`TrackingStore` split:
//! implemented by [`crate::MemoryStore`] for tests and by the Postgres store
//! in `camelmailer-db` for production (which enters the server's RLS tenant
//! context for every message-data query).
//!
//! The trait grows one bundle at a time as the Server API phases land; this
//! module starts with the request-scope newtype and the trait shell.

use crate::admin_store::StoreError;
use crate::message::{MessageRecord, QueuedMessage, SentMessage};
use crate::model::Id;
use async_trait::async_trait;

/// The server a per-server API request is scoped to, injected as a request
/// extension by the server-token auth middleware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerContext(pub Id);

/// Filter for listing messages (all fields optional). Pagination is applied
/// by the handler; this only narrows the result set.
#[derive(Debug, Clone, Default)]
pub struct MessageFilter {
    /// "incoming" or "outgoing".
    pub scope: Option<String>,
    pub status: Option<String>,
    pub tag: Option<String>,
    /// Case-insensitive substring match against subject or recipient.
    pub query: Option<String>,
}

/// A recorded open (pixel load) or click.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityEvent {
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    /// The clicked URL (clicks only).
    pub url: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A delivery attempt (read model).
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRecord {
    pub id: i64,
    pub status: String,
    pub details: Option<String>,
    pub output: Option<String>,
    pub sent_with_ssl: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Storage for the per-server API. Kept separate from [`crate::AdminStore`]
/// because these endpoints are authenticated by a server token and operate
/// only within one tenant.
///
/// Methods are added per phase (send, read, stats, streams, templates).
#[async_trait]
pub trait ServerStore: Send + Sync {
    /// Persist an accepted outbound message and enqueue it for delivery
    /// (the worker applies DKIM + tracking at delivery time). Returns the
    /// message's public identity.
    async fn store_outgoing(&self, message: QueuedMessage) -> Result<SentMessage, StoreError>;

    /// The server's messages matching `filter`, newest first. Tenant-scoped
    /// (RLS in Postgres; explicit `server_id` filter in memory).
    async fn messages(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError>;

    /// One message by id, or `None` if it does not belong to `server_id`.
    async fn message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError>;

    /// Delivery attempts for the message (empty if it isn't the server's).
    async fn deliveries(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<DeliveryRecord>, StoreError>;

    /// Recorded opens (pixel loads) for the message.
    async fn opens(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, StoreError>;

    /// Recorded link clicks for the message.
    async fn clicks(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, StoreError>;
}

#[async_trait]
impl ServerStore for crate::store::MemoryStore {
    async fn store_outgoing(&self, message: QueuedMessage) -> Result<SentMessage, StoreError> {
        Ok(self.insert_message_record(message))
    }

    async fn messages(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError> {
        Ok(self.messages_filtered(server_id, filter))
    }

    async fn message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(self.message_for(server_id, message_id))
    }

    async fn deliveries(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<DeliveryRecord>, StoreError> {
        Ok(self.deliveries_for(server_id, message_id))
    }

    async fn opens(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, StoreError> {
        Ok(self.opens_for(server_id, message_id))
    }

    async fn clicks(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, StoreError> {
        Ok(self.clicks_for(server_id, message_id))
    }
}
