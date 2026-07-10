//! Storage abstraction over the main database.
//!
//! The Ruby implementation reaches straight into ActiveRecord from the SMTP
//! server (`Credential.where(...)`, `Server.where(token: ...)` etc.). Here
//! those lookups live behind the [`Store`] trait, with [`MemoryStore`] as the
//! in-memory implementation used by tests, and a MariaDB-backed
//! implementation to follow.

use crate::model::*;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::RwLock;

pub trait Store: Send + Sync {
    fn organization(&self, id: Id) -> Option<Organization>;
    fn server(&self, id: Id) -> Option<Server>;

    /// `Credential.where(type: "SMTP", key: password).first`
    fn find_smtp_credential_by_key(&self, key: &str) -> Option<Credential>;

    /// `Server.where(token: token).first`
    fn find_server_by_token(&self, token: &str) -> Option<Server>;

    /// `Route.where(token: token).first`, joined with server + domain
    fn find_route_by_token(&self, token: &str) -> Option<ResolvedRoute>;

    /// `Route.find_by_name_and_domain(name, domain)`
    fn find_route_by_name_and_domain(&self, name: &str, domain: &str) -> Option<ResolvedRoute>;

    /// SMTP-IP credentials sorted by prefix length descending; first whose
    /// CIDR contains `ip` wins (IPv4-mapped IPv6 addresses match their IPv4
    /// CIDRs).
    fn find_ip_credential(&self, ip: IpAddr) -> Option<Credential>;

    /// All SMTP credentials for a server (used by CRAM-MD5).
    fn smtp_credentials_for_server(&self, server_id: Id) -> Vec<Credential>;

    /// `Server.includes(:organization).where(organizations: { permalink: .. }, permalink: ..)`
    fn find_server_by_permalinks(&self, org_permalink: &str, server_permalink: &str)
        -> Option<Server>;

    /// `server.find_authenticated_domain_from_headers` — given the values of
    /// the From (and, if the server allows it, Sender) headers, find a
    /// verified domain owned by the server or its organization which matches
    /// every address in the header. Returns the domain's id.
    fn find_authenticated_domain(&self, server_id: Id, header_values: &[&str]) -> Option<Id>;

    /// Does the server have a `__returnpath__` route?
    fn return_path_route_for_server(&self, server_id: Id) -> Option<ResolvedRoute>;

    /// Record a use of a credential (`credential.use` — bumps last_used_at).
    fn record_credential_use(&self, credential_id: Id);
}

/// Extract the bare address from a header value like `Name <addr@host>`
/// (ports `Postal::Helpers.strip_name_from_address`).
pub fn strip_name_from_address(value: &str) -> &str {
    if let (Some(start), Some(end)) = (value.find('<'), value.rfind('>')) {
        if start < end {
            return value[start + 1..end].trim();
        }
    }
    value.trim()
}

/// Longest-prefix matching of SMTP-IP credentials against a client address
/// (IPv4-mapped IPv6 clients match IPv4 CIDRs). Shared by the in-memory and
/// Postgres stores.
pub fn match_ip_credential(credentials: Vec<Credential>, ip: IpAddr) -> Option<Credential> {
    let mut candidates: Vec<(ipnet::IpNet, Credential)> = credentials
        .into_iter()
        .filter(|c| c.credential_type == CredentialType::SmtpIp)
        .filter_map(|c| {
            let net = c
                .key
                .parse::<ipnet::IpNet>()
                .or_else(|_| c.key.parse::<IpAddr>().map(ipnet::IpNet::from))
                .ok()?;
            Some((net, c))
        })
        .collect();
    // Longest prefix first, mirroring `sort_by { |c| c.ipaddr&.prefix }.reverse`
    candidates.sort_by(|a, b| b.0.prefix_len().cmp(&a.0.prefix_len()));
    candidates
        .into_iter()
        .find(|(net, _)| {
            if net.contains(&ip) {
                return true;
            }
            // an IPv4 CIDR should match the IPv4-mapped form of an IPv6 client
            if let (ipnet::IpNet::V4(net4), IpAddr::V6(ip6)) = (net, ip) {
                if let Some(mapped) = ip6.to_ipv4_mapped() {
                    return net4.contains(&mapped);
                }
            }
            false
        })
        .map(|(_, c)| c)
}

#[derive(Default)]
pub(crate) struct MemoryStoreInner {
    pub(crate) organizations: HashMap<Id, Organization>,
    pub(crate) servers: HashMap<Id, Server>,
    pub(crate) domains: HashMap<Id, Domain>,
    pub(crate) routes: HashMap<Id, Route>,
    pub(crate) credentials: HashMap<Id, Credential>,
    pub(crate) credential_uses: HashMap<Id, u64>,
    /// full key -> record (record holds only prefix for display; the map key
    /// is the secret used for validation).
    pub(crate) admin_api_keys: HashMap<String, AdminApiKey>,
    pub(crate) users: HashMap<Id, User>,
    pub(crate) ip_pools: HashMap<Id, IpPool>,
    pub(crate) ip_addresses: HashMap<Id, IpAddress>,
    pub(crate) webhooks: HashMap<Id, Webhook>,
    pub(crate) suppressions: HashMap<Id, Suppression>,
    /// HTTP-sent / stored messages, for the per-server read API tests.
    pub(crate) messages: Vec<crate::message::MessageRecord>,
    /// Delivery attempts keyed by message id (per-server read API tests).
    pub(crate) message_deliveries: Vec<(i64, crate::server_store::DeliveryRecord)>,
    /// Opens keyed by message id.
    pub(crate) message_opens: Vec<(i64, crate::server_store::ActivityEvent)>,
    /// Clicks keyed by message id.
    pub(crate) message_clicks: Vec<(i64, crate::server_store::ActivityEvent)>,
}

/// A thread-safe in-memory [`Store`].
#[derive(Default)]
pub struct MemoryStore {
    pub(crate) inner: RwLock<MemoryStoreInner>,
    next_id: AtomicU64,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            inner: RwLock::default(),
            next_id: AtomicU64::new(1),
        }
    }

    pub fn next_id(&self) -> Id {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    pub fn insert_organization(&self, organization: Organization) -> Organization {
        let mut inner = self.inner.write().unwrap();
        inner
            .organizations
            .insert(organization.id, organization.clone());
        organization
    }

    pub fn insert_server(&self, server: Server) -> Server {
        let mut inner = self.inner.write().unwrap();
        inner.servers.insert(server.id, server.clone());
        server
    }

    pub fn insert_domain(&self, domain: Domain) -> Domain {
        let mut inner = self.inner.write().unwrap();
        inner.domains.insert(domain.id, domain.clone());
        domain
    }

    pub fn insert_route(&self, route: Route) -> Route {
        let mut inner = self.inner.write().unwrap();
        inner.routes.insert(route.id, route.clone());
        route
    }

    pub fn insert_credential(&self, credential: Credential) -> Credential {
        let mut inner = self.inner.write().unwrap();
        inner.credentials.insert(credential.id, credential.clone());
        credential
    }

    pub fn delete_organization(&self, id: Id) -> bool {
        self.inner
            .write()
            .unwrap()
            .organizations
            .remove(&id)
            .is_some()
    }

    pub fn delete_server(&self, id: Id) -> bool {
        self.inner.write().unwrap().servers.remove(&id).is_some()
    }

    pub fn delete_domain(&self, id: Id) -> bool {
        self.inner.write().unwrap().domains.remove(&id).is_some()
    }

    pub fn organizations(&self) -> Vec<Organization> {
        let mut all: Vec<_> = self
            .inner
            .read()
            .unwrap()
            .organizations
            .values()
            .cloned()
            .collect();
        all.sort_by_key(|o| o.id);
        all
    }

    pub fn servers(&self) -> Vec<Server> {
        let mut all: Vec<_> = self
            .inner
            .read()
            .unwrap()
            .servers
            .values()
            .cloned()
            .collect();
        all.sort_by_key(|s| s.id);
        all
    }

    pub fn domains(&self) -> Vec<Domain> {
        let mut all: Vec<_> = self
            .inner
            .read()
            .unwrap()
            .domains
            .values()
            .cloned()
            .collect();
        all.sort_by_key(|d| d.id);
        all
    }

    pub fn domain(&self, id: Id) -> Option<Domain> {
        self.inner.read().unwrap().domains.get(&id).cloned()
    }

    pub fn insert_admin_api_key(&self, key: &str) -> AdminApiKey {
        self.insert_admin_api_key_named("api-key", key)
    }

    pub fn insert_admin_api_key_named(&self, name: &str, key: &str) -> AdminApiKey {
        let record = AdminApiKey {
            id: self.next_id(),
            uuid: crate::token::generate_uuid(),
            name: name.to_string(),
            key_prefix: key.chars().take(6).collect(),
        };
        self.inner
            .write()
            .unwrap()
            .admin_api_keys
            .insert(key.to_string(), record.clone());
        record
    }

    pub fn admin_api_key_exists(&self, key: &str) -> bool {
        self.inner.read().unwrap().admin_api_keys.contains_key(key)
    }

    /// Store an accepted outbound message as a read-model record and return
    /// its public identity (in-memory analogue of the Postgres insert path).
    pub fn insert_message_record(
        &self,
        message: crate::message::QueuedMessage,
    ) -> crate::message::SentMessage {
        let id = self.next_id() as i64;
        let token = crate::token::generate_token(12);
        let record = crate::message::MessageRecord {
            id,
            token: token.clone(),
            server_id: message.server_id,
            scope: match message.scope {
                crate::message::MessageScope::Incoming => "incoming".into(),
                crate::message::MessageScope::Outgoing => "outgoing".into(),
            },
            rcpt_to: message.rcpt_to.clone(),
            mail_from: message.mail_from,
            subject: crate::message::header_value(&message.raw_message, "subject"),
            message_id_header: crate::message::header_value(&message.raw_message, "message-id"),
            tag: message.tag,
            status: "Pending".into(),
            bounce: message.bounce,
            spam_status: "NotChecked".into(),
            spam_score: 0.0,
            held: false,
            threat: false,
            size: message.raw_message.len() as i64,
            metadata: message.metadata,
            created_at: chrono::Utc::now(),
            raw_message: message.raw_message,
        };
        self.inner.write().unwrap().messages.push(record);
        crate::message::SentMessage {
            id,
            token,
            rcpt_to: message.rcpt_to,
        }
    }

    /// Override a stored message's delivery status (test seeding).
    pub fn set_message_status(&self, message_id: i64, status: &str) {
        let mut inner = self.inner.write().unwrap();
        if let Some(message) = inner.messages.iter_mut().find(|m| m.id == message_id) {
            message.status = status.to_string();
            if status == "Bounced" {
                message.bounce = true;
            }
        }
    }

    /// All stored messages for a server (test read model).
    pub fn messages_for(&self, server_id: Id) -> Vec<crate::message::MessageRecord> {
        self.inner
            .read()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.server_id == server_id)
            .cloned()
            .collect()
    }

    /// Server's messages matching a filter, newest first (test read model).
    pub fn messages_filtered(
        &self,
        server_id: Id,
        filter: &crate::server_store::MessageFilter,
    ) -> Vec<crate::message::MessageRecord> {
        let query = filter.query.as_deref().map(str::to_lowercase);
        let mut matched: Vec<_> = self
            .inner
            .read()
            .unwrap()
            .messages
            .iter()
            .filter(|m| m.server_id == server_id)
            .filter(|m| filter.scope.as_deref().is_none_or(|s| m.scope == s))
            .filter(|m| filter.status.as_deref().is_none_or(|s| m.status == s))
            .filter(|m| {
                filter
                    .tag
                    .as_deref()
                    .is_none_or(|t| m.tag.as_deref() == Some(t))
            })
            .filter(|m| {
                query.as_deref().is_none_or(|q| {
                    m.subject
                        .as_deref()
                        .is_some_and(|s| s.to_lowercase().contains(q))
                        || m.rcpt_to.to_lowercase().contains(q)
                })
            })
            .cloned()
            .collect();
        matched.sort_by(|a, b| b.id.cmp(&a.id));
        matched
    }

    /// One message by id, scoped to the server (test read model).
    pub fn message_for(&self, server_id: Id, message_id: i64) -> Option<crate::message::MessageRecord> {
        self.inner
            .read()
            .unwrap()
            .messages
            .iter()
            .find(|m| m.server_id == server_id && m.id == message_id)
            .cloned()
    }

    fn message_belongs_to(&self, server_id: Id, message_id: i64) -> bool {
        self.inner
            .read()
            .unwrap()
            .messages
            .iter()
            .any(|m| m.server_id == server_id && m.id == message_id)
    }

    /// Attach a delivery attempt to a message (test seeding).
    pub fn insert_delivery_record(
        &self,
        message_id: i64,
        delivery: crate::server_store::DeliveryRecord,
    ) {
        self.inner
            .write()
            .unwrap()
            .message_deliveries
            .push((message_id, delivery));
    }

    /// Attach an open to a message (test seeding).
    pub fn insert_open_record(&self, message_id: i64, open: crate::server_store::ActivityEvent) {
        self.inner
            .write()
            .unwrap()
            .message_opens
            .push((message_id, open));
    }

    /// Attach a click to a message (test seeding).
    pub fn insert_click_record(&self, message_id: i64, click: crate::server_store::ActivityEvent) {
        self.inner
            .write()
            .unwrap()
            .message_clicks
            .push((message_id, click));
    }

    /// Delivery attempts for a message, tenant-scoped (test read model).
    pub fn deliveries_for(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Vec<crate::server_store::DeliveryRecord> {
        if !self.message_belongs_to(server_id, message_id) {
            return Vec::new();
        }
        self.inner
            .read()
            .unwrap()
            .message_deliveries
            .iter()
            .filter(|(id, _)| *id == message_id)
            .map(|(_, d)| d.clone())
            .collect()
    }

    /// Opens for a message, tenant-scoped (test read model).
    pub fn opens_for(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Vec<crate::server_store::ActivityEvent> {
        if !self.message_belongs_to(server_id, message_id) {
            return Vec::new();
        }
        self.inner
            .read()
            .unwrap()
            .message_opens
            .iter()
            .filter(|(id, _)| *id == message_id)
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// Clicks for a message, tenant-scoped (test read model).
    pub fn clicks_for(
        &self,
        server_id: Id,
        message_id: i64,
    ) -> Vec<crate::server_store::ActivityEvent> {
        if !self.message_belongs_to(server_id, message_id) {
            return Vec::new();
        }
        self.inner
            .read()
            .unwrap()
            .message_clicks
            .iter()
            .filter(|(id, _)| *id == message_id)
            .map(|(_, e)| e.clone())
            .collect()
    }

    /// Aggregate message + engagement counters for a server (test read model).
    pub fn message_stats_for(
        &self,
        server_id: Id,
        filter: &crate::server_store::StatsFilter,
    ) -> crate::server_store::MessageStats {
        use std::collections::HashSet;
        let inner = self.inner.read().unwrap();
        let ids: HashSet<i64> = inner
            .messages
            .iter()
            .filter(|m| m.server_id == server_id)
            .filter(|m| filter.from.is_none_or(|from| m.created_at >= from))
            .filter(|m| filter.to.is_none_or(|to| m.created_at <= to))
            .map(|m| m.id)
            .collect();

        let mut stats = crate::server_store::MessageStats::default();
        for message in inner.messages.iter().filter(|m| ids.contains(&m.id)) {
            stats.total += 1;
            match message.scope.as_str() {
                "incoming" => stats.incoming += 1,
                _ => stats.outgoing += 1,
            }
            match message.status.as_str() {
                "Sent" => stats.sent += 1,
                "Held" => stats.held += 1,
                "SoftFail" => stats.soft_fail += 1,
                "HardFail" => stats.hard_fail += 1,
                "Bounced" => stats.bounced += 1,
                "Pending" => stats.pending += 1,
                _ => {}
            }
        }

        let opens_for: HashSet<i64> = inner
            .message_opens
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(id, _)| *id)
            .collect();
        stats.opens = inner
            .message_opens
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .count() as i64;
        stats.unique_opens = opens_for.len() as i64;

        let clicks_for: HashSet<i64> = inner
            .message_clicks
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .map(|(id, _)| *id)
            .collect();
        stats.clicks = inner
            .message_clicks
            .iter()
            .filter(|(id, _)| ids.contains(id))
            .count() as i64;
        stats.unique_clicks = clicks_for.len() as i64;
        stats
    }

    /// Pending-outbound queue depth per destination domain (test read model,
    /// derived from `Pending` outgoing messages as the queue proxy).
    pub fn delivery_stats_for(&self, server_id: Id) -> crate::server_store::DeliveryStats {
        use std::collections::BTreeMap;
        let inner = self.inner.read().unwrap();
        let mut per_domain: BTreeMap<String, i64> = BTreeMap::new();
        for message in inner.messages.iter().filter(|m| {
            m.server_id == server_id && m.scope == "outgoing" && m.status == "Pending"
        }) {
            let domain = message
                .rcpt_to
                .rsplit_once('@')
                .map(|(_, d)| d.to_string())
                .unwrap_or_default();
            *per_domain.entry(domain).or_insert(0) += 1;
        }
        let queued = per_domain.values().sum();
        let domains = per_domain
            .into_iter()
            .map(|(domain, count)| crate::server_store::QueuedDomain { domain, count })
            .collect();
        crate::server_store::DeliveryStats { queued, domains }
    }

    pub fn list_admin_api_keys(&self) -> Vec<AdminApiKey> {
        let mut all: Vec<_> = self
            .inner
            .read()
            .unwrap()
            .admin_api_keys
            .values()
            .cloned()
            .collect();
        all.sort_by_key(|k| k.id);
        all
    }

    pub fn delete_admin_api_key(&self, id: Id) -> bool {
        let mut inner = self.inner.write().unwrap();
        let key = inner
            .admin_api_keys
            .iter()
            .find(|(_, record)| record.id == id)
            .map(|(k, _)| k.clone());
        key.map(|k| inner.admin_api_keys.remove(&k)).is_some()
    }

    pub fn credential_use_count(&self, credential_id: Id) -> u64 {
        self.inner
            .read()
            .unwrap()
            .credential_uses
            .get(&credential_id)
            .copied()
            .unwrap_or(0)
    }

    fn resolve_route(inner: &MemoryStoreInner, route: &Route) -> Option<ResolvedRoute> {
        let server = inner.servers.get(&route.server_id)?.clone();
        let domain_name = route
            .domain_id
            .and_then(|id| inner.domains.get(&id))
            .map(|d| d.name.clone())
            .unwrap_or_default();
        Some(ResolvedRoute {
            route: route.clone(),
            server,
            domain_name,
        })
    }
}

impl Store for MemoryStore {
    fn organization(&self, id: Id) -> Option<Organization> {
        self.inner.read().unwrap().organizations.get(&id).cloned()
    }

    fn server(&self, id: Id) -> Option<Server> {
        self.inner.read().unwrap().servers.get(&id).cloned()
    }

    fn find_smtp_credential_by_key(&self, key: &str) -> Option<Credential> {
        let inner = self.inner.read().unwrap();
        inner
            .credentials
            .values()
            .find(|c| c.credential_type == CredentialType::Smtp && c.key == key)
            .cloned()
    }

    fn find_server_by_token(&self, token: &str) -> Option<Server> {
        let inner = self.inner.read().unwrap();
        inner.servers.values().find(|s| s.token == token).cloned()
    }

    fn find_route_by_token(&self, token: &str) -> Option<ResolvedRoute> {
        let inner = self.inner.read().unwrap();
        let route = inner.routes.values().find(|r| r.token == token)?;
        Self::resolve_route(&inner, route)
    }

    fn find_route_by_name_and_domain(&self, name: &str, domain: &str) -> Option<ResolvedRoute> {
        let inner = self.inner.read().unwrap();
        let route = inner.routes.values().find(|r| {
            r.name == name
                && r.domain_id
                    .and_then(|id| inner.domains.get(&id))
                    .is_some_and(|d| d.name == domain)
        })?;
        Self::resolve_route(&inner, route)
    }

    fn find_ip_credential(&self, ip: IpAddr) -> Option<Credential> {
        let credentials: Vec<Credential> = {
            let inner = self.inner.read().unwrap();
            inner.credentials.values().cloned().collect()
        };
        match_ip_credential(credentials, ip)
    }

    fn smtp_credentials_for_server(&self, server_id: Id) -> Vec<Credential> {
        let inner = self.inner.read().unwrap();
        let mut credentials: Vec<_> = inner
            .credentials
            .values()
            .filter(|c| c.server_id == server_id && c.credential_type == CredentialType::Smtp)
            .cloned()
            .collect();
        credentials.sort_by_key(|c| c.id);
        credentials
    }

    fn find_server_by_permalinks(
        &self,
        org_permalink: &str,
        server_permalink: &str,
    ) -> Option<Server> {
        let inner = self.inner.read().unwrap();
        let organization = inner
            .organizations
            .values()
            .find(|o| o.permalink == org_permalink)?;
        inner
            .servers
            .values()
            .find(|s| s.organization_id == organization.id && s.permalink == server_permalink)
            .cloned()
    }

    fn find_authenticated_domain(&self, server_id: Id, header_values: &[&str]) -> Option<Id> {
        let inner = self.inner.read().unwrap();
        let server = inner.servers.get(&server_id)?;

        let domain_for_address = |address: &str| -> Option<Id> {
            let address = strip_name_from_address(address);
            let (uname, domain_name) = address.split_once('@')?;
            if uname.is_empty() {
                return None;
            }
            inner
                .domains
                .values()
                .filter(|d| d.verified && d.name == domain_name)
                .find(|d| match d.owner {
                    DomainOwner::Server(id) => id == server.id,
                    DomainOwner::Organization(id) => id == server.organization_id,
                })
                .map(|d| d.id)
        };

        // Every address in the header must authenticate; the first one's
        // domain is returned (mirrors find_authenticated_domain_from_headers).
        let domains: Vec<Option<Id>> = header_values
            .iter()
            .map(|value| domain_for_address(value))
            .collect();
        if !domains.is_empty() && domains.iter().all(Option::is_some) {
            return domains[0];
        }
        None
    }

    fn return_path_route_for_server(&self, server_id: Id) -> Option<ResolvedRoute> {
        let inner = self.inner.read().unwrap();
        let route = inner
            .routes
            .values()
            .find(|r| r.server_id == server_id && r.name == "__returnpath__")?;
        Self::resolve_route(&inner, route)
    }

    fn record_credential_use(&self, credential_id: Id) {
        let mut inner = self.inner.write().unwrap();
        *inner.credential_uses.entry(credential_id).or_insert(0) += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Fixtures;

    #[test]
    fn strip_name_from_address_handles_all_forms() {
        assert_eq!(strip_name_from_address("a@b.com"), "a@b.com");
        assert_eq!(strip_name_from_address("Name <a@b.com>"), "a@b.com");
        assert_eq!(strip_name_from_address("  a@b.com  "), "a@b.com");
        assert_eq!(
            strip_name_from_address("\"Last, First\" <a@b.com>"),
            "a@b.com"
        );
    }

    #[test]
    fn finds_smtp_credential_by_key() {
        let fixtures = Fixtures::new();
        let credential = fixtures.credential(CredentialType::Smtp, "secret-key");
        let store = fixtures.store();
        assert_eq!(
            store.find_smtp_credential_by_key("secret-key").unwrap().id,
            credential.id
        );
        assert!(store.find_smtp_credential_by_key("wrong").is_none());
    }

    #[test]
    fn ip_credential_prefers_longest_prefix_and_matches_mapped_ipv6() {
        let fixtures = Fixtures::new();
        let wide = fixtures.credential(CredentialType::SmtpIp, "1.0.0.0/8");
        let narrow = fixtures.credential(CredentialType::SmtpIp, "1.2.3.0/24");
        let store = fixtures.store();

        let found = store.find_ip_credential("1.2.3.4".parse().unwrap()).unwrap();
        assert_eq!(found.id, narrow.id);

        let found = store.find_ip_credential("1.9.9.9".parse().unwrap()).unwrap();
        assert_eq!(found.id, wide.id);

        let found = store
            .find_ip_credential("::ffff:1.2.3.4".parse().unwrap())
            .unwrap();
        assert_eq!(found.id, narrow.id);

        assert!(store.find_ip_credential("9.9.9.9".parse().unwrap()).is_none());
    }

    #[test]
    fn authenticated_domain_requires_all_addresses_to_match() {
        let fixtures = Fixtures::new();
        let domain = fixtures.verified_server_domain("example.com");
        let store = fixtures.store();
        let server_id = fixtures.server_id();

        assert_eq!(
            store.find_authenticated_domain(server_id, &["test@example.com"]),
            Some(domain.id)
        );
        assert_eq!(
            store.find_authenticated_domain(server_id, &["Name <test@example.com>"]),
            Some(domain.id)
        );
        assert_eq!(
            store.find_authenticated_domain(server_id, &["test@other.com"]),
            None
        );
        assert_eq!(
            store.find_authenticated_domain(
                server_id,
                &["test@example.com", "other@unverified.net"]
            ),
            None
        );
    }

    #[test]
    fn unverified_domains_do_not_authenticate() {
        let fixtures = Fixtures::new();
        fixtures.unverified_server_domain("example.com");
        let store = fixtures.store();
        assert_eq!(
            store.find_authenticated_domain(fixtures.server_id(), &["test@example.com"]),
            None
        );
    }

    #[test]
    fn route_lookup_by_name_and_domain() {
        let fixtures = Fixtures::new();
        let domain = fixtures.verified_server_domain("example.com");
        let route = fixtures.route("info", Some(domain.id), RouteMode::Endpoint);
        let store = fixtures.store();

        let resolved = store
            .find_route_by_name_and_domain("info", "example.com")
            .unwrap();
        assert_eq!(resolved.route.id, route.id);
        assert_eq!(resolved.domain_name, "example.com");
        assert!(store
            .find_route_by_name_and_domain("info", "wrong.com")
            .is_none());
    }
}
