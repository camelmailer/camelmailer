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
use crate::model::{Id, MessageStream, Template};
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
    /// Restrict to one message stream (resolved id).
    pub stream_id: Option<Id>,
}

/// Fields for creating a message stream.
#[derive(Debug, Clone)]
pub struct NewStream {
    pub server_id: Id,
    pub name: String,
    pub permalink: String,
    pub stream_type: String,
}

/// Fields for creating a message template.
#[derive(Debug, Clone)]
pub struct NewTemplate {
    pub server_id: Id,
    pub name: String,
    pub permalink: String,
    pub subject: Option<String>,
    pub html_body: Option<String>,
    pub text_body: Option<String>,
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

/// Optional time window for statistics (`created_at` bounds, inclusive).
#[derive(Debug, Clone, Default)]
pub struct StatsFilter {
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

/// Aggregate message/engagement counters for a server (a time window).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessageStats {
    pub total: i64,
    pub incoming: i64,
    pub outgoing: i64,
    pub sent: i64,
    pub held: i64,
    pub soft_fail: i64,
    pub hard_fail: i64,
    pub bounced: i64,
    pub pending: i64,
    /// Total open (pixel-load) events.
    pub opens: i64,
    /// Messages with at least one open.
    pub unique_opens: i64,
    /// Total click events.
    pub clicks: i64,
    /// Messages with at least one click.
    pub unique_clicks: i64,
}

/// Outbound queue depth per destination domain.
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedDomain {
    pub domain: String,
    pub count: i64,
}

/// Snapshot of the server's pending outbound delivery queue.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DeliveryStats {
    pub queued: i64,
    pub domains: Vec<QueuedDomain>,
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
    async fn opens(&self, server_id: Id, message_id: i64)
        -> Result<Vec<ActivityEvent>, StoreError>;

    /// Recorded link clicks for the message.
    async fn clicks(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Vec<ActivityEvent>, StoreError>;

    /// Aggregate message + engagement counters over an optional time window.
    async fn message_stats(
        &self,
        server_id: Id,
        filter: &StatsFilter,
    ) -> Result<MessageStats, StoreError>;

    /// Pending outbound queue depth (total + per destination domain).
    async fn delivery_stats(&self, server_id: Id) -> Result<DeliveryStats, StoreError>;

    /// Bounced messages (bounce flag or `Bounced` status), newest first.
    /// Reuses [`MessageFilter`] for the optional substring/tag narrowing.
    async fn bounces(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError>;

    /// One bounced message by id, or `None` if it isn't the server's or
    /// isn't a bounce.
    async fn bounce(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError>;

    // message streams (config; server-scoped)
    async fn list_streams(&self, server_id: Id) -> Result<Vec<MessageStream>, StoreError>;
    async fn stream_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<MessageStream>, StoreError>;
    async fn create_stream(&self, new: NewStream) -> Result<MessageStream, StoreError>;
    async fn update_stream(&self, stream: MessageStream) -> Result<MessageStream, StoreError>;

    // inbound message management (scope = incoming)
    /// Incoming messages matching `filter` (scope is always forced to
    /// incoming), newest first.
    async fn inbound_messages(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError>;

    /// One incoming message by id, or `None` if it isn't the server's or
    /// isn't incoming.
    async fn inbound_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError>;

    /// Re-queue an incoming message for processing with block rules bypassed
    /// (sets `bypassed`, resets status to Pending, enqueues). `None` if the
    /// message isn't the server's incoming message.
    async fn bypass_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError>;

    /// Re-queue an incoming message for processing (resets status to Pending,
    /// enqueues) without bypassing rules.
    async fn retry_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError>;

    // templates (config; server-scoped)
    async fn list_templates(&self, server_id: Id) -> Result<Vec<Template>, StoreError>;
    async fn template_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<Template>, StoreError>;
    async fn create_template(&self, new: NewTemplate) -> Result<Template, StoreError>;
    async fn update_template(&self, template: Template) -> Result<Template, StoreError>;
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

    async fn message_stats(
        &self,
        server_id: Id,
        filter: &StatsFilter,
    ) -> Result<MessageStats, StoreError> {
        Ok(self.message_stats_for(server_id, filter))
    }

    async fn delivery_stats(&self, server_id: Id) -> Result<DeliveryStats, StoreError> {
        Ok(self.delivery_stats_for(server_id))
    }

    async fn bounces(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError> {
        Ok(self
            .messages_filtered(server_id, filter)
            .into_iter()
            .filter(|m| m.bounce || m.status == "Bounced")
            .collect())
    }

    async fn bounce(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(self
            .message_for(server_id, message_id)
            .filter(|m| m.bounce || m.status == "Bounced"))
    }

    async fn list_streams(&self, server_id: Id) -> Result<Vec<MessageStream>, StoreError> {
        Ok(self.streams_for(server_id))
    }

    async fn stream_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<MessageStream>, StoreError> {
        Ok(self.find_stream(server_id, permalink))
    }

    async fn create_stream(&self, new: NewStream) -> Result<MessageStream, StoreError> {
        if self.find_stream(new.server_id, &new.permalink).is_some() {
            return Err(StoreError::Conflict(
                "Permalink has already been taken".into(),
            ));
        }
        Ok(self.insert_stream(MessageStream {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            server_id: new.server_id,
            name: new.name,
            permalink: new.permalink,
            stream_type: new.stream_type,
            archived: false,
        }))
    }

    async fn update_stream(&self, stream: MessageStream) -> Result<MessageStream, StoreError> {
        Ok(self.insert_stream(stream))
    }

    async fn inbound_messages(
        &self,
        server_id: Id,
        filter: &MessageFilter,
    ) -> Result<Vec<MessageRecord>, StoreError> {
        let filter = MessageFilter {
            scope: Some("incoming".into()),
            ..filter.clone()
        };
        Ok(self.messages_filtered(server_id, &filter))
    }

    async fn inbound_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(self
            .message_for(server_id, message_id)
            .filter(|m| m.scope == "incoming"))
    }

    async fn bypass_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(self.requeue_inbound(server_id, message_id, true))
    }

    async fn retry_message(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Result<Option<MessageRecord>, StoreError> {
        Ok(self.requeue_inbound(server_id, message_id, false))
    }

    async fn list_templates(&self, server_id: Id) -> Result<Vec<Template>, StoreError> {
        Ok(self.templates_for(server_id))
    }

    async fn template_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<Template>, StoreError> {
        Ok(self.find_template(server_id, permalink))
    }

    async fn create_template(&self, new: NewTemplate) -> Result<Template, StoreError> {
        if self.find_template(new.server_id, &new.permalink).is_some() {
            return Err(StoreError::Conflict(
                "Permalink has already been taken".into(),
            ));
        }
        Ok(self.insert_template(Template {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            server_id: new.server_id,
            name: new.name,
            permalink: new.permalink,
            subject: new.subject,
            html_body: new.html_body,
            text_body: new.text_body,
            archived: false,
        }))
    }

    async fn update_template(&self, template: Template) -> Result<Template, StoreError> {
        Ok(self.insert_template(template))
    }
}
