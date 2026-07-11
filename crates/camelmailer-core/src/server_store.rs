//! The tenant-scoped storage interface behind the per-server API
//! (`/api/v2/server/...`). Mirrors the `AdminStore`/`TrackingStore` split:
//! implemented by [`crate::MemoryStore`] for tests and by the Postgres store
//! in `camelmailer-db` for production (which enters the server's RLS tenant
//! context for every message-data query).
//!
//! The trait grows one bundle at a time as the Server API phases land; this
//! module starts with the request-scope newtype and the trait shell.

use crate::admin_store::StoreError;
use crate::dmarc::{DmarcFilter, DmarcRecordRow, DmarcReport, NewDmarcReport};
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
}
