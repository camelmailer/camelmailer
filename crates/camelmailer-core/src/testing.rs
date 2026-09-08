//! Test fixtures shared across crates (the Rust counterpart of the
//! FactoryBot factories in `spec/factories`).

use crate::message::{MessageScope, MessageSink, QueueMessagesOutcome, QueuedMessage};
use crate::model::*;
use crate::store::MemoryStore;
use crate::token;
use std::sync::Arc;

/// Builds a [`MemoryStore`] pre-populated with one organization and one
/// server, plus helpers to add related records — mirroring
/// `create(:server)` / `create(:credential, ...)` in the Ruby specs.
pub struct Fixtures {
    store: Arc<MemoryStore>,
    organization: Organization,
    server: Server,
}

impl Default for Fixtures {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixtures {
    pub fn new() -> Self {
        let store = Arc::new(MemoryStore::new());
        let organization = store.insert_organization(Organization {
            id: store.next_id(),
            uuid: token::generate_uuid(),
            name: "Example Org".into(),
            permalink: "example-org".into(),
            require_two_factor: false,
        });
        let server = store.insert_server(Server {
            id: store.next_id(),
            uuid: token::generate_uuid(),
            organization_id: organization.id,
            name: "Example Server".into(),
            permalink: "example-server".into(),
            token: token::generate_token(6),
            mode: ServerMode::Live,
            suspended: false,
            suspension_reason: None,
            privacy_mode: false,
            log_smtp_data: false,
            allow_sender: false,
            send_limit: None,
            ip_pool_id: None,
            track_opens: false,
            track_clicks: false,
            spam_threshold: None,
            outbound_spam_threshold: None,
            bounce_hook_url: None,
            delivery_hook_url: None,
            inbound_domain: None,
            broadcast_physical_address: None,
            color: None,
            default_stream_id: None,
        });
        Self {
            store,
            organization,
            server,
        }
    }

    pub fn store(&self) -> Arc<MemoryStore> {
        self.store.clone()
    }

    pub fn organization(&self) -> &Organization {
        &self.organization
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    pub fn server_id(&self) -> Id {
        self.server.id
    }

    pub fn suspend_server(&self) {
        let mut server = self.server.clone();
        server.suspended = true;
        server.suspension_reason = Some("Suspended for testing".into());
        self.store.insert_server(server);
    }

    /// Give the fixture server a send limit (`None` for unlimited).
    pub fn set_send_limit(&self, limit: Option<i64>) {
        let mut server = self.server.clone();
        server.send_limit = limit;
        self.store.insert_server(server);
    }

    /// Count `count` outgoing messages against the server's current window,
    /// without going through the send path.
    pub fn record_sends(&self, count: usize) {
        for index in 0..count {
            self.store
                .insert_message_record(crate::message::QueuedMessage {
                    server_id: self.server.id,
                    rcpt_to: format!("seed{index}@dest.example"),
                    mail_from: "sender@example.com".into(),
                    raw_message: b"Subject: seed\r\n\r\nx".to_vec(),
                    received_with_ssl: false,
                    scope: crate::message::MessageScope::Outgoing,
                    bounce: false,
                    domain_id: None,
                    credential_id: None,
                    route_id: None,
                    tag: None,
                    metadata: None,
                    stream_id: None,
                });
        }
    }

    pub fn set_privacy_mode(&self, enabled: bool) {
        let mut server = self.server.clone();
        server.privacy_mode = enabled;
        self.store.insert_server(server);
    }

    pub fn credential(&self, credential_type: CredentialType, key: &str) -> Credential {
        self.store.insert_credential(Credential {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            server_id: self.server.id,
            credential_type,
            name: "Test Credential".into(),
            key: key.into(),
            hold: false,
            last_used_at: None,
        })
    }

    pub fn verified_server_domain(&self, name: &str) -> Domain {
        self.store.insert_domain(Domain {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            owner: DomainOwner::Server(self.server.id),
            name: name.into(),
            verified: true,
            verification_token: token::generate_token(32),
            check_dmarc: true,
            check_spf: true,
            dkim_private_key: None,
        })
    }

    pub fn unverified_server_domain(&self, name: &str) -> Domain {
        self.store.insert_domain(Domain {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            owner: DomainOwner::Server(self.server.id),
            name: name.into(),
            verified: false,
            verification_token: token::generate_token(32),
            check_dmarc: true,
            check_spf: true,
            dkim_private_key: None,
        })
    }

    pub fn route(&self, name: &str, domain_id: Option<Id>, mode: RouteMode) -> Route {
        self.store.insert_route(Route {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            server_id: self.server.id,
            domain_id,
            name: name.into(),
            token: token::generate_token(8),
            mode,
            endpoint_url: None,
        })
    }

    /// A sender address of the fixture server, confirmed or pending.
    pub fn sender_address(&self, email: &str, verified: bool) -> SenderAddress {
        self.store.insert_sender_address(SenderAddress {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            server_id: self.server.id,
            email_address: email.into(),
            verified,
            verification_token_hash: (!verified).then(|| token::generate_token(8)),
        })
    }

    pub fn route_with_endpoint(
        &self,
        name: &str,
        domain_id: Option<Id>,
        endpoint_url: &str,
    ) -> Route {
        self.store.insert_route(Route {
            id: self.store.next_id(),
            uuid: token::generate_uuid(),
            server_id: self.server.id,
            domain_id,
            name: name.into(),
            token: token::generate_token(8),
            mode: RouteMode::Endpoint,
            endpoint_url: Some(endpoint_url.into()),
        })
    }
}

/// A [`MessageSink`] that stores through to a [`MemoryStore`], the way
/// `PgMessageSink` stores through to PostgreSQL.
///
/// [`crate::MemorySink`] collects messages in its own `Vec` and never touches
/// the store, so anything the real sink does *as part of storing* does not
/// happen: today that is incrementing the per-server send counters. A test
/// driving an SMTP `Session` against `MemorySink` therefore reads
/// `Store::send_usage` as 0 no matter how many messages it just accepted,
/// which made a send-limit bypass across transactions unobservable (the
/// limit was cached for the session while the per-transaction reset cleared
/// the recipient count, so a client holding one connection open could send
/// past its `send_limit` without bound; shipped in v0.7.7, fixed in v0.7.8).
///
/// Use this sink whenever a test asserts on state that storing a message
/// produces, rather than on the messages themselves.
///
/// Idempotency claims are deliberately unsupported: replay and conflict
/// semantics are transactional and are covered against real PostgreSQL in
/// `pg_tests.rs`. Passing a claim here panics rather than quietly behaving
/// differently from production.
pub struct StoreBackedSink {
    store: Arc<MemoryStore>,
}

impl StoreBackedSink {
    pub fn new(store: Arc<MemoryStore>) -> Self {
        Self { store }
    }
}

impl MessageSink for StoreBackedSink {
    fn queue_message(&self, message: QueuedMessage) {
        self.store.insert_message_record(message);
    }

    fn queue_messages(
        &self,
        messages: Vec<QueuedMessage>,
        idempotency: Option<crate::server_store::IdempotencyRequest>,
        allowance: crate::SendAllowance,
    ) -> QueueMessagesOutcome {
        assert!(
            idempotency.is_none(),
            "StoreBackedSink does not implement idempotent replay; \
             test that against PostgreSQL in pg_tests.rs"
        );
        let outgoing = messages
            .iter()
            .filter(|message| message.scope == MessageScope::Outgoing)
            .count() as i64;
        if !allowance.allows(outgoing) {
            return QueueMessagesOutcome::LimitExceeded(allowance.rejection_message());
        }
        for message in messages {
            self.store.insert_message_record(message);
        }
        QueueMessagesOutcome::Stored
    }
}
