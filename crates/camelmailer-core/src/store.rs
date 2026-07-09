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

#[derive(Default)]
struct MemoryStoreInner {
    organizations: HashMap<Id, Organization>,
    servers: HashMap<Id, Server>,
    domains: HashMap<Id, Domain>,
    routes: HashMap<Id, Route>,
    credentials: HashMap<Id, Credential>,
    credential_uses: HashMap<Id, u64>,
}

/// A thread-safe in-memory [`Store`].
#[derive(Default)]
pub struct MemoryStore {
    inner: RwLock<MemoryStoreInner>,
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
        let inner = self.inner.read().unwrap();
        let mut candidates: Vec<(ipnet::IpNet, &Credential)> = inner
            .credentials
            .values()
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
            .map(|(_, c)| c.clone())
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
