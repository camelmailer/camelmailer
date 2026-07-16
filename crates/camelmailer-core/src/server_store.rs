//! The tenant-scoped storage interface behind the per-server API
//! (`/api/v2/server/...`). Mirrors the `AdminStore`/`TrackingStore` split:
//! implemented by [`crate::MemoryStore`] for tests and by the Postgres store
//! in `camelmailer-db` for production (which enters the server's RLS tenant
//! context for every message-data query).
//!
//! The trait grows one bundle at a time as the Server API phases land; this
//! module starts with the request-scope newtype and the trait shell.

use crate::admin_store::{AdminStore, NewSuppression, StoreError};
use crate::dmarc::{DmarcFilter, DmarcRecordRow, DmarcReport, NewDmarcReport};
use crate::message::{MessageRecord, QueuedMessage, SentMessage};
use crate::model::{Campaign, Id, MessageStream, NewCampaign, Subscription, Template};
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
    /// IP pool the stream sources outbound mail from (`None` = the server's
    /// pool).
    pub ip_pool_id: Option<Id>,
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
    /// Layout to wrap the rendered bodies in (None = no layout).
    pub layout_id: Option<Id>,
}

/// Fields for creating a layout (see [`crate::model::Layout`]).
#[derive(Debug, Clone)]
pub struct NewLayout {
    pub server_id: Id,
    pub name: String,
    pub permalink: String,
    pub html_wrapper: String,
    pub text_wrapper: Option<String>,
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

/// Optional time window for statistics (`created_at` bounds, inclusive),
/// plus an optional tag to scope every counter to.
#[derive(Debug, Clone, Default)]
pub struct StatsFilter {
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
    /// Restrict all counters to messages carrying exactly this tag.
    pub tag: Option<String>,
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
    /// Bounce breakdown: messages classified as hard bounces.
    pub bounces_hard: i64,
    /// Bounce breakdown: messages classified as soft bounces.
    pub bounces_soft: i64,
    /// Bounce breakdown: unclassified bounces (category `undetermined`,
    /// plus bounce-flagged / `Bounced` messages without a category).
    pub bounces_undetermined: i64,
}

/// Per-campaign analytics, aggregated over the messages a campaign produced
/// (attributed via `messages.campaign_id`) plus their loads/link_clicks and
/// the stream's suppressions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CampaignStats {
    /// Recipient count captured at creation (from the campaign row).
    pub total: i64,
    /// Recipients expanded into messages so far (from the campaign row).
    pub sent: i64,
    /// Attributed messages with status `Sent` and not held.
    pub delivered: i64,
    /// Attributed messages that failed (`Bounced`/`HardFail`, or bounce flag).
    pub failed: i64,
    /// Distinct attributed messages with at least one open (load).
    pub opened: i64,
    /// Distinct attributed messages with at least one click.
    pub clicked: i64,
    /// Stream-scoped suppressions (`unsubscribe`/`complaint`) of the campaign's
    /// stream created at or after the campaign's `created_at`.
    pub unsubscribed: i64,
}

/// One tag used by a server's messages, with its message count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagCount {
    pub tag: String,
    pub count: i64,
}

/// One logged API request (the read model of the `api_requests` table).
/// Deliberately metadata-only: no bodies, no keys, no query strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiRequestRecord {
    pub id: i64,
    pub server_id: Id,
    pub method: String,
    /// Request path without the query string.
    pub path: String,
    pub status_code: i32,
    pub duration_ms: i64,
    /// Truncated to 255 characters at write time.
    pub user_agent: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Fields for logging one API request.
#[derive(Debug, Clone)]
pub struct NewApiRequest {
    pub server_id: Id,
    pub method: String,
    pub path: String,
    pub status_code: i32,
    pub duration_ms: i64,
    pub user_agent: Option<String>,
}

/// Filter for listing logged API requests (all fields optional).
#[derive(Debug, Clone, Default)]
pub struct ApiRequestFilter {
    /// Status-code class: 2 matches 200–299, 4 matches 400–499, …
    pub status_class: Option<i32>,
    /// Exact HTTP method (case-insensitive at the API edge, stored upper).
    pub method: Option<String>,
    pub from: Option<chrono::DateTime<chrono::Utc>>,
    pub to: Option<chrono::DateTime<chrono::Utc>>,
}

impl ApiRequestFilter {
    /// Does a logged request match this filter? Shared by the in-memory
    /// store; the Postgres store expresses the same predicate in SQL.
    pub fn matches(&self, record: &ApiRequestRecord) -> bool {
        self.status_class
            .is_none_or(|class| record.status_code / 100 == class)
            && self
                .method
                .as_deref()
                .is_none_or(|m| record.method.eq_ignore_ascii_case(m))
            && self.from.is_none_or(|from| record.created_at >= from)
            && self.to.is_none_or(|to| record.created_at <= to)
    }
}

/// A public share link for one message. Only the SHA-256 hash of the share
/// token is ever stored; the unauthenticated share endpoint resolves the
/// presented token by hash (a cross-tenant lookup like tracking tokens).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageShare {
    pub id: i64,
    pub server_id: Id,
    pub message_id: i64,
    /// SHA-256 hex of the share token (never the token itself).
    pub token_hash: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// Fields for creating a message share link.
#[derive(Debug, Clone)]
pub struct NewMessageShare {
    pub server_id: Id,
    pub message_id: i64,
    pub token_hash: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
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

    /// The tags used by the server's messages since `since`, with counts,
    /// most used first (ties by tag name). Tenant-scoped (RLS in Postgres).
    async fn tags(
        &self,
        server_id: Id,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<TagCount>, StoreError>;

    // API request log (tenant-scoped by the server_id column; queries
    // always filter on it)
    /// Persist one request-log entry. Called fire-and-forget by the
    /// middleware — failures must never surface to the request.
    async fn record_api_request(&self, new: NewApiRequest) -> Result<(), StoreError>;

    /// The server's logged requests matching `filter`, newest first.
    async fn api_requests(
        &self,
        server_id: Id,
        filter: &ApiRequestFilter,
    ) -> Result<Vec<ApiRequestRecord>, StoreError>;

    /// Delete request-log entries (of every server) created before
    /// `older_than`; returns how many were removed. Worker housekeeping.
    async fn prune_api_requests(
        &self,
        older_than: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, StoreError>;

    // message streams (config; server-scoped)
    async fn list_streams(&self, server_id: Id) -> Result<Vec<MessageStream>, StoreError>;
    async fn stream_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<MessageStream>, StoreError>;
    async fn create_stream(&self, new: NewStream) -> Result<MessageStream, StoreError>;
    async fn update_stream(&self, stream: MessageStream) -> Result<MessageStream, StoreError>;

    /// The source address for a message on `stream_id`: the highest-priority
    /// IPv4 (as a string) of the stream's IP pool when the stream sets one,
    /// else of the server's pool, else `None`. With `stream_id = None` (or a
    /// stream without a pool) this resolves EXACTLY to the server-pool address
    /// the worker used before per-stream pools existed — no regression for
    /// transactional streams. Server-scoped (RLS-free config lookup).
    async fn source_ip_for(
        &self,
        server_id: Id,
        stream_id: Option<Id>,
    ) -> Result<Option<String>, StoreError>;

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

    // layouts (config; server-scoped)
    async fn list_layouts(&self, server_id: Id) -> Result<Vec<crate::model::Layout>, StoreError>;
    async fn layout_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<crate::model::Layout>, StoreError>;
    async fn layout_by_id(
        &self,
        server_id: Id,
        layout_id: Id,
    ) -> Result<Option<crate::model::Layout>, StoreError>;
    async fn create_layout(&self, new: NewLayout) -> Result<crate::model::Layout, StoreError>;
    async fn update_layout(
        &self,
        layout: crate::model::Layout,
    ) -> Result<crate::model::Layout, StoreError>;
    async fn delete_layout(&self, server_id: Id, layout_id: Id) -> Result<bool, StoreError>;

    // DMARC aggregate reports (tenant-scoped: RLS in Postgres)
    /// Persist one parsed aggregate report with all its rows.
    async fn store_dmarc_report(&self, new: NewDmarcReport) -> Result<DmarcReport, StoreError>;

    /// The server's reports matching `filter`, newest range first.
    async fn dmarc_reports(
        &self,
        server_id: Id,
        filter: &DmarcFilter,
    ) -> Result<Vec<DmarcReport>, StoreError>;

    /// One report with its rows, or `None` if it isn't the server's.
    async fn dmarc_report(
        &self,
        server_id: Id,
        report_id: i64,
    ) -> Result<Option<(DmarcReport, Vec<DmarcRecordRow>)>, StoreError>;

    /// All rows of the server's reports matching `filter` — the input of
    /// [`crate::dmarc::summarize`].
    async fn dmarc_records(
        &self,
        server_id: Id,
        filter: &DmarcFilter,
    ) -> Result<Vec<DmarcRecordRow>, StoreError>;

    // message share links (cross-tenant lookup by token hash)
    /// Persist a share link (`new.token_hash` is the SHA-256 of the token;
    /// the token itself is never stored). Creation is server-scoped: the
    /// caller must have resolved the message within its own server first.
    async fn create_message_share(&self, new: NewMessageShare) -> Result<MessageShare, StoreError>;

    /// Resolve a presented share token (by hash) to its share record —
    /// deliberately NOT server-scoped: the public share endpoint has no
    /// tenant context until this lookup provides one. Expiry is enforced
    /// by the caller.
    async fn message_share_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<MessageShare>, StoreError>;

    // suppression gate + one-click unsubscribe (broadcast streams)
    /// Is `address` suppressed for a message on `stream_id`? True when a
    /// suppression row exists for the address that is either server-wide
    /// (`stream_id IS NULL`) or scoped to this exact stream. Tenant-scoped
    /// (RLS in Postgres; explicit `server_id` filter in memory).
    async fn address_suppressed(
        &self,
        server_id: Id,
        address: &str,
        stream_id: Option<Id>,
    ) -> Result<bool, StoreError>;

    /// Register a one-click unsubscribe token for `address` on `stream_id`
    /// and return the opaque token. Cross-tenant lookup table (resolved by
    /// token alone by the unauthenticated unsubscribe endpoint).
    async fn create_unsubscribe_token(
        &self,
        server_id: Id,
        stream_id: Option<Id>,
        address: &str,
    ) -> Result<String, StoreError>;

    /// Resolve an unsubscribe token to `(server_id, stream_id, address)`, or
    /// `None` when the token is unknown.
    async fn resolve_unsubscribe_token(
        &self,
        token: &str,
    ) -> Result<Option<(Id, Option<Id>, String)>, StoreError>;

    /// Act on a presented unsubscribe token: resolve it, then create a
    /// stream-scoped `unsubscribe` suppression AND flip the matching
    /// subscription to `unsubscribed` (idempotent — an existing suppression is
    /// not an error). Returns whether a token matched.
    async fn record_unsubscribe(&self, token: &str) -> Result<bool, StoreError>;

    // opt-in / consent (broadcast streams; tenant-scoped)
    /// The stream's subscription rows (opt-ins and opt-outs), ordered by id.
    async fn list_subscriptions(
        &self,
        server_id: Id,
        stream_id: Id,
    ) -> Result<Vec<Subscription>, StoreError>;

    /// Insert a subscription for `(server, stream, address)`, or update its
    /// status when one already exists. Returns the resulting row.
    async fn upsert_subscription(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
        status: &str,
    ) -> Result<Subscription, StoreError>;

    /// Remove the subscription for `(server, stream, address)`. Returns whether
    /// a row was deleted.
    async fn remove_subscription(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<bool, StoreError>;

    /// Has `address` opted in to `stream_id`? True iff a row exists with status
    /// `subscribed`. The broadcast opt-in send gate.
    async fn is_subscribed(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<bool, StoreError>;

    /// Record a spam complaint for `(server, stream, address)`: create a
    /// stream-scoped `complaint` suppression AND flip the matching
    /// subscription to `unsubscribed` (idempotent — an existing suppression is
    /// not an error). The manual FBL/spam-complaint mechanism; mirrors
    /// [`Self::record_unsubscribe`] but keyed directly on the address rather
    /// than a one-click token. Returns the resulting subscription row.
    async fn record_complaint(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<Subscription, StoreError>;

    // broadcast campaigns (tenant-scoped: RLS in Postgres)
    /// Record a campaign in status `sending` (with `sent = 0`) and return it.
    /// The background expansion attributes messages and advances progress.
    async fn create_campaign(&self, new: NewCampaign) -> Result<Campaign, StoreError>;

    /// One campaign by id, or `None` if it isn't the server's.
    async fn get_campaign(&self, server_id: Id, id: Id) -> Result<Option<Campaign>, StoreError>;

    /// The stream's campaigns, newest first.
    async fn list_campaigns(
        &self,
        server_id: Id,
        stream_id: Id,
    ) -> Result<Vec<Campaign>, StoreError>;

    /// Update a campaign's progress: set `sent`, `status`, and (when finished)
    /// `completed_at`. Called by the background expansion per batch and at the
    /// end. No-op when the campaign isn't the server's.
    async fn set_campaign_progress(
        &self,
        server_id: Id,
        id: Id,
        sent: i64,
        status: &str,
        completed: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), StoreError>;

    /// Attribute a stored message to a campaign (`messages.campaign_id`). Used
    /// by the expansion after each per-recipient send. No-op when the message
    /// isn't the server's.
    async fn set_message_campaign(
        &self,
        server_id: Id,
        message_id: i64,
        campaign_id: Id,
    ) -> Result<(), StoreError>;

    /// Aggregate analytics for one campaign over its attributed messages plus
    /// their loads/link_clicks and the stream's suppressions. Returns a zeroed
    /// [`CampaignStats`] when the campaign isn't the server's.
    async fn campaign_stats(
        &self,
        server_id: Id,
        campaign_id: Id,
    ) -> Result<CampaignStats, StoreError>;
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

    async fn tags(
        &self,
        server_id: Id,
        since: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<TagCount>, StoreError> {
        Ok(self.tags_for(server_id, since))
    }

    async fn record_api_request(&self, new: NewApiRequest) -> Result<(), StoreError> {
        self.insert_api_request(new);
        Ok(())
    }

    async fn api_requests(
        &self,
        server_id: Id,
        filter: &ApiRequestFilter,
    ) -> Result<Vec<ApiRequestRecord>, StoreError> {
        Ok(self.api_requests_for(server_id, filter))
    }

    async fn prune_api_requests(
        &self,
        older_than: chrono::DateTime<chrono::Utc>,
    ) -> Result<u64, StoreError> {
        Ok(self.prune_api_requests_before(older_than))
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
            ip_pool_id: new.ip_pool_id,
        }))
    }

    async fn update_stream(&self, stream: MessageStream) -> Result<MessageStream, StoreError> {
        Ok(self.insert_stream(stream))
    }

    async fn source_ip_for(
        &self,
        server_id: Id,
        stream_id: Option<Id>,
    ) -> Result<Option<String>, StoreError> {
        let inner = self.inner.read().unwrap();
        // Resolve the stream's pool first (if the stream is set and carries
        // one), else the server's pool — the exact fallback the worker relied
        // on before per-stream pools existed.
        let pool_id = stream_id
            .and_then(|id| inner.message_streams.get(&id))
            .filter(|s| s.server_id == server_id)
            .and_then(|s| s.ip_pool_id)
            .or_else(|| inner.servers.get(&server_id).and_then(|s| s.ip_pool_id));
        let Some(pool_id) = pool_id else {
            return Ok(None);
        };
        // Highest-priority (lowest priority number, tie by id) address in the
        // pool — the same ordering the Postgres store applies.
        Ok(inner
            .ip_addresses
            .values()
            .filter(|a| a.ip_pool_id == pool_id)
            .min_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)))
            .map(|a| a.ipv4.clone()))
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
            layout_id: new.layout_id,
        }))
    }

    async fn update_template(&self, template: Template) -> Result<Template, StoreError> {
        Ok(self.insert_template(template))
    }

    async fn list_layouts(&self, server_id: Id) -> Result<Vec<crate::model::Layout>, StoreError> {
        Ok(self.layouts_for(server_id))
    }

    async fn layout_by_permalink(
        &self,
        server_id: Id,
        permalink: &str,
    ) -> Result<Option<crate::model::Layout>, StoreError> {
        Ok(self.find_layout(server_id, permalink))
    }

    async fn layout_by_id(
        &self,
        server_id: Id,
        layout_id: Id,
    ) -> Result<Option<crate::model::Layout>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .layouts
            .get(&layout_id)
            .filter(|l| l.server_id == server_id)
            .cloned())
    }

    async fn create_layout(&self, new: NewLayout) -> Result<crate::model::Layout, StoreError> {
        if self.find_layout(new.server_id, &new.permalink).is_some() {
            return Err(StoreError::Conflict(
                "Permalink has already been taken".into(),
            ));
        }
        Ok(self.insert_layout(crate::model::Layout {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            server_id: new.server_id,
            name: new.name,
            permalink: new.permalink,
            html_wrapper: new.html_wrapper,
            text_wrapper: new.text_wrapper,
        }))
    }

    async fn update_layout(
        &self,
        layout: crate::model::Layout,
    ) -> Result<crate::model::Layout, StoreError> {
        Ok(self.insert_layout(layout))
    }

    async fn delete_layout(&self, server_id: Id, layout_id: Id) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let existed = inner
            .layouts
            .get(&layout_id)
            .is_some_and(|l| l.server_id == server_id);
        if existed {
            inner.layouts.remove(&layout_id);
            // Mirror the Postgres FK: templates lose the reference, they
            // are not deleted.
            for template in inner.templates.values_mut() {
                if template.layout_id == Some(layout_id) {
                    template.layout_id = None;
                }
            }
        }
        Ok(existed)
    }

    async fn store_dmarc_report(&self, new: NewDmarcReport) -> Result<DmarcReport, StoreError> {
        Ok(self.insert_dmarc_report(new))
    }

    async fn dmarc_reports(
        &self,
        server_id: Id,
        filter: &DmarcFilter,
    ) -> Result<Vec<DmarcReport>, StoreError> {
        Ok(self.dmarc_reports_for(server_id, filter))
    }

    async fn dmarc_report(
        &self,
        server_id: Id,
        report_id: i64,
    ) -> Result<Option<(DmarcReport, Vec<DmarcRecordRow>)>, StoreError> {
        Ok(self.dmarc_report_for(server_id, report_id))
    }

    async fn dmarc_records(
        &self,
        server_id: Id,
        filter: &DmarcFilter,
    ) -> Result<Vec<DmarcRecordRow>, StoreError> {
        Ok(self.dmarc_records_for(server_id, filter))
    }

    async fn create_message_share(&self, new: NewMessageShare) -> Result<MessageShare, StoreError> {
        Ok(self.insert_message_share(new))
    }

    async fn message_share_by_token_hash(
        &self,
        token_hash: &str,
    ) -> Result<Option<MessageShare>, StoreError> {
        Ok(self.find_message_share(token_hash))
    }

    async fn address_suppressed(
        &self,
        server_id: Id,
        address: &str,
        stream_id: Option<Id>,
    ) -> Result<bool, StoreError> {
        Ok(self.inner.read().unwrap().suppressions.values().any(|s| {
            s.server_id == server_id
                && s.address == address
                && (s.stream_id.is_none() || s.stream_id == stream_id)
        }))
    }

    async fn create_unsubscribe_token(
        &self,
        server_id: Id,
        stream_id: Option<Id>,
        address: &str,
    ) -> Result<String, StoreError> {
        let token = crate::token::generate_token(32);
        self.inner.write().unwrap().unsubscribe_tokens.push((
            token.clone(),
            server_id,
            stream_id,
            address.to_string(),
        ));
        Ok(token)
    }

    async fn resolve_unsubscribe_token(
        &self,
        token: &str,
    ) -> Result<Option<(Id, Option<Id>, String)>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .unsubscribe_tokens
            .iter()
            .find(|(t, ..)| t == token)
            .map(|(_, server_id, stream_id, address)| (*server_id, *stream_id, address.clone())))
    }

    async fn record_unsubscribe(&self, token: &str) -> Result<bool, StoreError> {
        let Some((server_id, stream_id, address)) = self.resolve_unsubscribe_token(token).await?
        else {
            return Ok(false);
        };
        // Idempotent: a duplicate stream-scoped suppression is not an error.
        match self
            .create_suppression(NewSuppression {
                server_id,
                suppression_type: "unsubscribe".into(),
                address: address.clone(),
                reason: Some("Unsubscribed via List-Unsubscribe".into()),
                stream_id,
            })
            .await
        {
            Ok(_) | Err(StoreError::Conflict(_)) => {}
            Err(error) => return Err(error),
        }
        // Flip the opt-in to `unsubscribed` for the stream this token targets
        // (subscriptions always belong to a concrete stream).
        if let Some(stream_id) = stream_id {
            self.upsert_subscription(server_id, stream_id, &address, "unsubscribed")
                .await?;
        }
        Ok(true)
    }

    async fn list_subscriptions(
        &self,
        server_id: Id,
        stream_id: Id,
    ) -> Result<Vec<Subscription>, StoreError> {
        let mut subscriptions: Vec<Subscription> = self
            .inner
            .read()
            .unwrap()
            .subscriptions
            .values()
            .filter(|s| s.server_id == server_id && s.stream_id == stream_id)
            .cloned()
            .collect();
        subscriptions.sort_by_key(|s| s.id);
        Ok(subscriptions)
    }

    async fn upsert_subscription(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
        status: &str,
    ) -> Result<Subscription, StoreError> {
        let mut inner = self.inner.write().unwrap();
        if let Some(existing) = inner
            .subscriptions
            .values_mut()
            .find(|s| s.server_id == server_id && s.stream_id == stream_id && s.address == address)
        {
            existing.status = status.to_string();
            return Ok(existing.clone());
        }
        drop(inner);
        let id = self.next_id();
        let subscription = Subscription {
            id,
            server_id,
            stream_id,
            address: address.to_string(),
            status: status.to_string(),
            created_at: Some(chrono::Utc::now()),
        };
        self.inner
            .write()
            .unwrap()
            .subscriptions
            .insert(id, subscription.clone());
        Ok(subscription)
    }

    async fn remove_subscription(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let id = inner
            .subscriptions
            .values()
            .find(|s| s.server_id == server_id && s.stream_id == stream_id && s.address == address)
            .map(|s| s.id);
        Ok(id.map(|id| inner.subscriptions.remove(&id)).is_some())
    }

    async fn is_subscribed(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.inner.read().unwrap().subscriptions.values().any(|s| {
            s.server_id == server_id
                && s.stream_id == stream_id
                && s.address == address
                && s.status == "subscribed"
        }))
    }

    async fn record_complaint(
        &self,
        server_id: Id,
        stream_id: Id,
        address: &str,
    ) -> Result<Subscription, StoreError> {
        // Idempotent: a duplicate stream-scoped suppression is not an error
        // (mirrors record_unsubscribe).
        match self
            .create_suppression(NewSuppression {
                server_id,
                suppression_type: "complaint".into(),
                address: address.to_string(),
                reason: Some("Marked as spam".into()),
                stream_id: Some(stream_id),
            })
            .await
        {
            Ok(_) | Err(StoreError::Conflict(_)) => {}
            Err(error) => return Err(error),
        }
        // Flip the opt-in closed so the send gate rejects future broadcasts.
        self.upsert_subscription(server_id, stream_id, address, "unsubscribed")
            .await
    }

    async fn create_campaign(&self, new: NewCampaign) -> Result<Campaign, StoreError> {
        Ok(self.insert_campaign(new))
    }

    async fn get_campaign(&self, server_id: Id, id: Id) -> Result<Option<Campaign>, StoreError> {
        Ok(self
            .inner
            .read()
            .unwrap()
            .campaigns
            .get(&id)
            .filter(|c| c.server_id == server_id)
            .cloned())
    }

    async fn list_campaigns(
        &self,
        server_id: Id,
        stream_id: Id,
    ) -> Result<Vec<Campaign>, StoreError> {
        let mut campaigns: Vec<Campaign> = self
            .inner
            .read()
            .unwrap()
            .campaigns
            .values()
            .filter(|c| c.server_id == server_id && c.stream_id == stream_id)
            .cloned()
            .collect();
        campaigns.sort_by_key(|c| std::cmp::Reverse(c.id));
        Ok(campaigns)
    }

    async fn set_campaign_progress(
        &self,
        server_id: Id,
        id: Id,
        sent: i64,
        status: &str,
        completed: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if let Some(campaign) = inner.campaigns.get_mut(&id) {
            if campaign.server_id == server_id {
                campaign.sent = sent;
                campaign.status = status.to_string();
                campaign.completed_at = completed;
            }
        }
        Ok(())
    }

    async fn set_message_campaign(
        &self,
        server_id: Id,
        message_id: i64,
        campaign_id: Id,
    ) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        if inner
            .messages
            .iter()
            .any(|m| m.id == message_id && m.server_id == server_id)
        {
            inner.message_campaigns.insert(message_id, campaign_id);
        }
        Ok(())
    }

    async fn campaign_stats(
        &self,
        server_id: Id,
        campaign_id: Id,
    ) -> Result<CampaignStats, StoreError> {
        use std::collections::HashSet;
        let inner = self.inner.read().unwrap();
        let Some(campaign) = inner
            .campaigns
            .get(&campaign_id)
            .filter(|c| c.server_id == server_id)
        else {
            return Ok(CampaignStats::default());
        };

        // Messages attributed to this campaign (tenant-scoped).
        let ids: HashSet<i64> = inner
            .message_campaigns
            .iter()
            .filter(|(_, cid)| **cid == campaign_id)
            .map(|(mid, _)| *mid)
            .collect();

        let mut delivered = 0i64;
        let mut failed = 0i64;
        for message in inner
            .messages
            .iter()
            .filter(|m| m.server_id == server_id && ids.contains(&m.id))
        {
            if message.status == "Sent" && !message.held {
                delivered += 1;
            }
            if message.status == "Bounced" || message.status == "HardFail" || message.bounce {
                failed += 1;
            }
        }

        let opened = inner
            .message_opens
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(id, _)| *id)
            .collect::<HashSet<i64>>()
            .len() as i64;
        let clicked = inner
            .message_clicks
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(id, _)| *id)
            .collect::<HashSet<i64>>()
            .len() as i64;

        // Stream-scoped unsubscribe/complaint suppressions of the campaign's
        // stream. (In-memory suppressions carry no timestamp, so the
        // created-at cut-off the Postgres store applies is not modelled here.)
        let unsubscribed = inner
            .suppressions
            .values()
            .filter(|s| {
                s.server_id == server_id
                    && s.stream_id == Some(campaign.stream_id)
                    && matches!(s.suppression_type.as_str(), "unsubscribe" | "complaint")
            })
            .count() as i64;

        Ok(CampaignStats {
            total: campaign.total,
            sent: campaign.sent,
            delivered,
            failed,
            opened,
            clicked,
            unsubscribed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bounce::BounceCategory;
    use crate::message::{MessageScope, QueuedMessage};
    use crate::store::MemoryStore;
    use chrono::{Duration, Utc};

    fn queued(server_id: Id, tag: Option<&str>) -> QueuedMessage {
        QueuedMessage {
            server_id,
            rcpt_to: "to@example.com".into(),
            mail_from: "from@example.com".into(),
            raw_message: b"Subject: T\r\n\r\nx\r\n".to_vec(),
            received_with_ssl: false,
            scope: MessageScope::Outgoing,
            bounce: false,
            domain_id: None,
            credential_id: None,
            route_id: None,
            tag: tag.map(str::to_string),
            metadata: None,
            stream_id: None,
        }
    }

    #[tokio::test]
    async fn tags_are_counted_windowed_and_tenant_scoped() {
        let store = MemoryStore::new();
        store.insert_message_record(queued(1, Some("welcome")));
        store.insert_message_record(queued(1, Some("welcome")));
        store.insert_message_record(queued(1, Some("promo")));
        store.insert_message_record(queued(1, None));
        store.insert_message_record(queued(2, Some("other")));
        // a stale tag outside the window is not listed
        let stale = store.insert_message_record(queued(1, Some("stale"))).id;
        store.set_message_created_at(stale, Utc::now() - Duration::days(40));

        let since = Utc::now() - Duration::days(30);
        let tags = ServerStore::tags(&store, 1, since).await.unwrap();
        assert_eq!(
            tags,
            vec![
                TagCount {
                    tag: "welcome".into(),
                    count: 2
                },
                TagCount {
                    tag: "promo".into(),
                    count: 1
                },
            ]
        );

        // tenant scoping: server 2 sees only its own tag
        let other = ServerStore::tags(&store, 2, since).await.unwrap();
        assert_eq!(
            other,
            vec![TagCount {
                tag: "other".into(),
                count: 1
            }]
        );
    }

    #[tokio::test]
    async fn stats_scope_to_a_tag_and_break_bounces_down_by_category() {
        let store = MemoryStore::new();
        let sent = store.insert_message_record(queued(1, Some("t1"))).id;
        store.set_message_status(sent, "Sent");
        // terminal 5xx failure classified hard
        let hard = store.insert_message_record(queued(1, Some("t1"))).id;
        store.set_message_status(hard, "HardFail");
        store.set_bounce_category(hard, BounceCategory::Hard);
        // exhausted 4xx failure classified soft
        let soft = store.insert_message_record(queued(1, Some("t2"))).id;
        store.set_message_status(soft, "HardFail");
        store.set_bounce_category(soft, BounceCategory::Soft);
        // an unclassified bounce counts as undetermined
        let dsn = store.insert_message_record(queued(1, None)).id;
        store.set_message_status(dsn, "Bounced");

        let all = ServerStore::message_stats(&store, 1, &StatsFilter::default())
            .await
            .unwrap();
        assert_eq!(all.total, 4);
        assert_eq!(all.bounces_hard, 1);
        assert_eq!(all.bounces_soft, 1);
        assert_eq!(all.bounces_undetermined, 1);

        let t1 = ServerStore::message_stats(
            &store,
            1,
            &StatsFilter {
                tag: Some("t1".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(t1.total, 2);
        assert_eq!(t1.sent, 1);
        assert_eq!(t1.hard_fail, 1);
        assert_eq!(t1.bounces_hard, 1);
        assert_eq!(t1.bounces_soft, 0);
        assert_eq!(t1.bounces_undetermined, 0);

        let missing = ServerStore::message_stats(
            &store,
            1,
            &StatsFilter {
                tag: Some("missing".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(missing.total, 0);
    }

    #[tokio::test]
    async fn api_request_log_records_filters_scopes_and_prunes() {
        let store = MemoryStore::new();
        let entry = |server_id: Id, method: &str, status: i32| NewApiRequest {
            server_id,
            method: method.into(),
            path: "/api/v2/server/messages".into(),
            status_code: status,
            duration_ms: 12,
            user_agent: Some("test-agent".into()),
        };
        ServerStore::record_api_request(&store, entry(1, "GET", 200))
            .await
            .unwrap();
        ServerStore::record_api_request(&store, entry(1, "POST", 404))
            .await
            .unwrap();
        ServerStore::record_api_request(&store, entry(2, "GET", 200))
            .await
            .unwrap();

        // newest first, scoped to the server
        let all = ServerStore::api_requests(&store, 1, &ApiRequestFilter::default())
            .await
            .unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].method, "POST");
        assert_eq!(all[0].path, "/api/v2/server/messages");

        // status-class filter (4 = 4xx)
        let four = ServerStore::api_requests(
            &store,
            1,
            &ApiRequestFilter {
                status_class: Some(4),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(four.len(), 1);
        assert_eq!(four[0].status_code, 404);

        // method filter is case-insensitive
        let gets = ServerStore::api_requests(
            &store,
            1,
            &ApiRequestFilter {
                method: Some("get".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(gets.len(), 1);

        // the foreign server sees only its own entry
        let other = ServerStore::api_requests(&store, 2, &ApiRequestFilter::default())
            .await
            .unwrap();
        assert_eq!(other.len(), 1);

        // retention: entries older than the cutoff are pruned
        let old_id = all[1].id;
        store.set_api_request_created_at(old_id, Utc::now() - Duration::days(31));
        let removed = ServerStore::prune_api_requests(&store, Utc::now() - Duration::days(30))
            .await
            .unwrap();
        assert_eq!(removed, 1);
        let remaining = ServerStore::api_requests(&store, 1, &ApiRequestFilter::default())
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, all[0].id);
    }

    #[tokio::test]
    async fn stream_scoped_suppression_blocks_only_its_stream() {
        let store = MemoryStore::new();
        // Suppress addr on stream 10 only.
        store
            .create_suppression(NewSuppression {
                server_id: 1,
                suppression_type: "unsubscribe".into(),
                address: "a@dest.example".into(),
                reason: None,
                stream_id: Some(10),
            })
            .await
            .unwrap();

        // Blocked on the matching stream, not on another stream, not server-wide.
        assert!(
            ServerStore::address_suppressed(&store, 1, "a@dest.example", Some(10))
                .await
                .unwrap()
        );
        assert!(
            !ServerStore::address_suppressed(&store, 1, "a@dest.example", Some(20))
                .await
                .unwrap()
        );
        assert!(
            !ServerStore::address_suppressed(&store, 1, "a@dest.example", None)
                .await
                .unwrap()
        );
        // Not another tenant's problem.
        assert!(
            !ServerStore::address_suppressed(&store, 2, "a@dest.example", Some(10))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn server_wide_suppression_blocks_every_stream() {
        let store = MemoryStore::new();
        store
            .create_suppression(NewSuppression {
                server_id: 1,
                suppression_type: "recipient".into(),
                address: "b@dest.example".into(),
                reason: Some("hard bounce".into()),
                stream_id: None,
            })
            .await
            .unwrap();

        assert!(
            ServerStore::address_suppressed(&store, 1, "b@dest.example", None)
                .await
                .unwrap()
        );
        assert!(
            ServerStore::address_suppressed(&store, 1, "b@dest.example", Some(10))
                .await
                .unwrap()
        );
        assert!(
            ServerStore::address_suppressed(&store, 1, "b@dest.example", Some(99))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn unsubscribe_token_roundtrips_and_record_is_idempotent() {
        let store = MemoryStore::new();
        let token = ServerStore::create_unsubscribe_token(&store, 1, Some(7), "c@dest.example")
            .await
            .unwrap();

        let resolved = ServerStore::resolve_unsubscribe_token(&store, &token)
            .await
            .unwrap();
        assert_eq!(resolved, Some((1, Some(7), "c@dest.example".to_string())));
        assert_eq!(
            ServerStore::resolve_unsubscribe_token(&store, "nope")
                .await
                .unwrap(),
            None
        );

        // Recording creates the stream-scoped suppression; unknown token → false.
        assert!(ServerStore::record_unsubscribe(&store, &token)
            .await
            .unwrap());
        assert!(!ServerStore::record_unsubscribe(&store, "nope")
            .await
            .unwrap());
        // Idempotent: recording the same token again is still Ok(true), no dup.
        assert!(ServerStore::record_unsubscribe(&store, &token)
            .await
            .unwrap());

        assert!(
            ServerStore::address_suppressed(&store, 1, "c@dest.example", Some(7))
                .await
                .unwrap()
        );
        // Only scoped to stream 7 — a transactional stream is unaffected.
        assert!(
            !ServerStore::address_suppressed(&store, 1, "c@dest.example", Some(1))
                .await
                .unwrap()
        );
        let suppressions = store.list_suppressions(1).await.unwrap();
        assert_eq!(suppressions.len(), 1);
        assert_eq!(suppressions[0].suppression_type, "unsubscribe");
        assert_eq!(suppressions[0].stream_id, Some(7));
    }

    #[tokio::test]
    async fn subscriptions_upsert_list_remove_and_gate_sends() {
        let store = MemoryStore::new();

        // Opt in, and idempotently upsert to the same status.
        let created =
            ServerStore::upsert_subscription(&store, 1, 7, "a@dest.example", "subscribed")
                .await
                .unwrap();
        assert_eq!(created.status, "subscribed");
        assert_eq!(created.stream_id, 7);
        assert!(ServerStore::is_subscribed(&store, 1, 7, "a@dest.example")
            .await
            .unwrap());
        // Not opted in to another stream, not another tenant.
        assert!(!ServerStore::is_subscribed(&store, 1, 8, "a@dest.example")
            .await
            .unwrap());
        assert!(!ServerStore::is_subscribed(&store, 2, 7, "a@dest.example")
            .await
            .unwrap());

        // Upsert flips status in place (no duplicate row).
        let flipped =
            ServerStore::upsert_subscription(&store, 1, 7, "a@dest.example", "unsubscribed")
                .await
                .unwrap();
        assert_eq!(flipped.id, created.id);
        assert_eq!(flipped.status, "unsubscribed");
        assert!(!ServerStore::is_subscribed(&store, 1, 7, "a@dest.example")
            .await
            .unwrap());

        // A second address, then list is stream-scoped and ordered by id.
        ServerStore::upsert_subscription(&store, 1, 7, "b@dest.example", "subscribed")
            .await
            .unwrap();
        let list = ServerStore::list_subscriptions(&store, 1, 7).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].address, "a@dest.example");
        assert_eq!(list[1].address, "b@dest.example");

        // Remove is a boolean; a second remove is a no-op.
        assert!(
            ServerStore::remove_subscription(&store, 1, 7, "a@dest.example")
                .await
                .unwrap()
        );
        assert!(
            !ServerStore::remove_subscription(&store, 1, 7, "a@dest.example")
                .await
                .unwrap()
        );
        assert_eq!(
            ServerStore::list_subscriptions(&store, 1, 7)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn record_unsubscribe_flips_the_subscription_to_unsubscribed() {
        let store = MemoryStore::new();
        // Opt in on stream 7, then unsubscribe via a one-click token.
        ServerStore::upsert_subscription(&store, 1, 7, "c@dest.example", "subscribed")
            .await
            .unwrap();
        assert!(ServerStore::is_subscribed(&store, 1, 7, "c@dest.example")
            .await
            .unwrap());

        let token = ServerStore::create_unsubscribe_token(&store, 1, Some(7), "c@dest.example")
            .await
            .unwrap();
        assert!(ServerStore::record_unsubscribe(&store, &token)
            .await
            .unwrap());

        // The subscription is now unsubscribed (the send gate closes) AND a
        // stream-scoped suppression exists.
        assert!(!ServerStore::is_subscribed(&store, 1, 7, "c@dest.example")
            .await
            .unwrap());
        let subscriptions = ServerStore::list_subscriptions(&store, 1, 7).await.unwrap();
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(subscriptions[0].status, "unsubscribed");
        assert!(
            ServerStore::address_suppressed(&store, 1, "c@dest.example", Some(7))
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn stream_ip_pool_round_trips_and_resolves_source_ip() {
        use crate::admin_store::{AdminStore, NewIpAddress};

        let fixtures = crate::testing::Fixtures::new();
        let store = fixtures.store();
        let server_id = fixtures.server_id();

        // The server's own pool (today's behaviour): two addresses, so the
        // lowest priority number must win.
        let server_pool = store.create_ip_pool("server", false).await.unwrap();
        store
            .create_ip_address(NewIpAddress {
                ip_pool_id: server_pool.id,
                ipv4: "10.0.0.9".into(),
                ipv6: None,
                hostname: "b.example".into(),
                priority: 5,
            })
            .await
            .unwrap();
        store
            .create_ip_address(NewIpAddress {
                ip_pool_id: server_pool.id,
                ipv4: "10.0.0.1".into(),
                ipv6: None,
                hostname: "a.example".into(),
                priority: 0,
            })
            .await
            .unwrap();
        store
            .set_server_ip_pool(server_id, Some(server_pool.id))
            .await
            .unwrap();

        // A separate broadcast pool for the stream to source from.
        let stream_pool = store.create_ip_pool("stream", false).await.unwrap();
        store
            .create_ip_address(NewIpAddress {
                ip_pool_id: stream_pool.id,
                ipv4: "10.0.0.2".into(),
                ipv6: None,
                hostname: "c.example".into(),
                priority: 0,
            })
            .await
            .unwrap();

        // create_stream with an ip_pool_id round-trips (and persists).
        let with_pool = store
            .create_stream(NewStream {
                server_id,
                name: "Broadcast".into(),
                permalink: "broadcast".into(),
                stream_type: "broadcast".into(),
                ip_pool_id: Some(stream_pool.id),
            })
            .await
            .unwrap();
        assert_eq!(with_pool.ip_pool_id, Some(stream_pool.id));
        assert_eq!(
            store
                .stream_by_permalink(server_id, "broadcast")
                .await
                .unwrap()
                .unwrap()
                .ip_pool_id,
            Some(stream_pool.id)
        );

        let without_pool = store
            .create_stream(NewStream {
                server_id,
                name: "Transactional".into(),
                permalink: "outbound".into(),
                stream_type: "transactional".into(),
                ip_pool_id: None,
            })
            .await
            .unwrap();
        assert_eq!(without_pool.ip_pool_id, None);

        // The stream's own pool wins when set.
        assert_eq!(
            ServerStore::source_ip_for(&*store, server_id, Some(with_pool.id))
                .await
                .unwrap(),
            Some("10.0.0.2".to_string())
        );
        // A stream without a pool falls back to the server pool (highest
        // priority = lowest priority number).
        assert_eq!(
            ServerStore::source_ip_for(&*store, server_id, Some(without_pool.id))
                .await
                .unwrap(),
            Some("10.0.0.1".to_string())
        );
        // No stream at all resolves to the server pool exactly as before.
        assert_eq!(
            ServerStore::source_ip_for(&*store, server_id, None)
                .await
                .unwrap(),
            Some("10.0.0.1".to_string())
        );
        // A server with no pool resolves to None.
        assert_eq!(
            ServerStore::source_ip_for(&*store, 999_999, None)
                .await
                .unwrap(),
            None
        );
    }
}
