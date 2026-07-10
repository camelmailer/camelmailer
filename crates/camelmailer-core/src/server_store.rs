//! The tenant-scoped storage interface behind the per-server API
//! (`/api/v2/server/...`). Mirrors the `AdminStore`/`TrackingStore` split:
//! implemented by [`crate::MemoryStore`] for tests and by the Postgres store
//! in `camelmailer-db` for production (which enters the server's RLS tenant
//! context for every message-data query).
//!
//! The trait grows one bundle at a time as the Server API phases land; this
//! module starts with the request-scope newtype and the trait shell.

use crate::admin_store::StoreError;
use crate::message::{QueuedMessage, SentMessage};
use crate::model::Id;
use async_trait::async_trait;

/// The server a per-server API request is scoped to, injected as a request
/// extension by the server-token auth middleware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerContext(pub Id);

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
}

#[async_trait]
impl ServerStore for crate::store::MemoryStore {
    async fn store_outgoing(&self, message: QueuedMessage) -> Result<SentMessage, StoreError> {
        Ok(self.insert_message_record(message))
    }
}
