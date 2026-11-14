//! The production DNS resolvers of the checks: domain verification
//! ([`HickoryDnsResolver`]), the domain health check's SPF evaluation
//! ([`HickorySpfResolver`]) and the infrastructure DNS checks
//! ([`HickoryDnsCheckResolver`]). All of them ask the public resolvers of
//! `dns.check_resolvers` (default 1.1.1.1 and 9.9.9.9), never the host's or
//! container's resolver, which can answer from local configuration (the
//! host name, `/etc/hosts`, mDNS, a split-horizon zone) instead of what the
//! rest of the internet sees. Tests inject
//! [`camelmailer_core::StaticDnsResolver`] or a fake instead.

use async_trait::async_trait;
use camelmailer_core::{DnsError, DnsResolver};
use hickory_resolver::config::{ResolveHosts, ResolverConfig};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::NetError;
use hickory_resolver::proto::rr::RData;
use hickory_resolver::TokioResolver;
use std::net::SocketAddr;

/// The default public resolvers (`dns.check_resolvers`).
pub fn default_check_resolvers() -> Vec<SocketAddr> {
    camelmailer_config::DEFAULT_CHECK_RESOLVERS
        .iter()
        .map(|ip| SocketAddr::new(ip.parse().expect("a valid default resolver address"), 53))
        .collect()
}

/// A resolver that asks only `servers` (UDP, then TCP): no system
/// configuration, no hosts file, no search domains. Every name is looked up
/// as given (the callers append the root dot).
pub fn public_resolver(servers: &[SocketAddr]) -> TokioResolver {
    let servers = if servers.is_empty() {
        default_check_resolvers()
    } else {
        servers.to_vec()
    };
    let mut config = ResolverConfig::from_parts(None, Vec::new(), Vec::new());
    for server in &servers {
        let mut name_server = hickory_resolver::config::NameServerConfig::udp_and_tcp(server.ip());
        for connection in &mut name_server.connections {
            connection.port = server.port();
        }
        config.add_name_server(name_server);
    }
    let mut builder = TokioResolver::builder_with_config(config, TokioRuntimeProvider::default());
    let options = builder.options_mut();
    options.use_hosts_file = ResolveHosts::Never;
    options.ndots = 0;
    options.timeout = std::time::Duration::from_secs(3);
    options.attempts = 2;
    builder
        .build()
        .expect("a resolver with a static configuration always builds")
}

/// Domain verification (`POST …/domains/{name}/verify`): TXT and CNAME
/// lookups through the public resolvers.
pub struct HickoryDnsResolver {
    servers: Vec<SocketAddr>,
}

impl HickoryDnsResolver {
    pub fn new(servers: Vec<SocketAddr>) -> Self {
        Self { servers }
    }
}

impl Default for HickoryDnsResolver {
    fn default() -> Self {
        Self::new(default_check_resolvers())
    }
}

#[async_trait]
impl DnsResolver for HickoryDnsResolver {
    async fn txt_records(&self, name: &str) -> Result<Vec<String>, DnsError> {
        let resolver = public_resolver(&self.servers);
        match resolver.txt_lookup(format!("{name}.")).await {
            Ok(lookup) => Ok(txt_strings(&lookup)),
            // "no such record" is an answer, not a failure
            Err(error) if error.is_no_records_found() => Ok(Vec::new()),
            Err(error) => Err(DnsError::Lookup(error.to_string())),
        }
    }

    async fn cname(&self, name: &str) -> Result<Option<String>, DnsError> {
        use hickory_resolver::proto::rr::RecordType;
        let resolver = public_resolver(&self.servers);
        match resolver.lookup(format!("{name}."), RecordType::CNAME).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .find_map(|record| match &record.data {
                    RData::CNAME(target) => {
                        Some(target.0.to_string().trim_end_matches('.').to_string())
                    }
                    _ => None,
                })),
            // "no such record" is an answer, not a failure
            Err(error) if error.is_no_records_found() => Ok(None),
            Err(error) => Err(DnsError::Lookup(error.to_string())),
        }
    }
}

/// The production [`SpfResolver`], used by the domain health check to
/// evaluate a sending domain's SPF the way a receiver would.
///
/// Unlike [`HickoryDnsResolver`] above, the underlying resolver is built once
/// and reused: one SPF evaluation issues up to the RFC's ten queries, and a
/// health check runs one evaluation per sending address, so rebuilding the
/// resolver per lookup would multiply setup cost by a factor of thirty or
/// more.
pub struct HickorySpfResolver {
    resolver: TokioResolver,
}

impl HickorySpfResolver {
    /// Asks the public resolvers `servers` (`dns.check_resolvers`).
    pub fn new(servers: &[SocketAddr]) -> Self {
        Self {
            resolver: public_resolver(servers),
        }
    }
}

impl Default for HickorySpfResolver {
    fn default() -> Self {
        Self::new(&default_check_resolvers())
    }
}

/// "No such record" is an answer (an empty set), any other failure is a
/// lookup error, which SPF turns into a temperror rather than a verdict.
fn empty_on_no_records<T>(error: NetError, empty: T) -> Result<T, DnsError> {
    if error.is_no_records_found() {
        Ok(empty)
    } else {
        Err(DnsError::Lookup(error.to_string()))
    }
}

/// The TXT strings of a lookup. A TXT record may be split into several
/// character strings; verifiers (and SPF) concatenate them with no separator.
fn txt_strings(lookup: &hickory_resolver::lookup::Lookup) -> Vec<String> {
    lookup
        .answers()
        .iter()
        .filter_map(|record| match &record.data {
            RData::TXT(txt) => Some(
                txt.txt_data
                    .iter()
                    .map(|data| String::from_utf8_lossy(data))
                    .collect::<String>(),
            ),
            _ => None,
        })
        .collect()
}

#[async_trait]
impl camelmailer_core::SpfResolver for HickorySpfResolver {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        match self.resolver.txt_lookup(format!("{name}.")).await {
            Ok(lookup) => Ok(txt_strings(&lookup)),
            Err(error) => empty_on_no_records(error, Vec::new()),
        }
    }

    async fn ip_addresses(&self, name: &str) -> Result<Vec<std::net::IpAddr>, DnsError> {
        match self.resolver.lookup_ip(format!("{name}.")).await {
            Ok(lookup) => Ok(lookup.iter().collect()),
            Err(error) => empty_on_no_records(error, Vec::new()),
        }
    }

    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError> {
        match self.resolver.mx_lookup(format!("{name}.")).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::MX(mx) => {
                        Some(mx.exchange.to_string().trim_end_matches('.').to_string())
                    }
                    _ => None,
                })
                .collect()),
            Err(error) => empty_on_no_records(error, Vec::new()),
        }
    }
}

/// Memoizes one SPF resolver for the span of a single health check.
///
/// A domain is evaluated once per sending address, and every one of those
/// walks the same record tree: the same TXT, A and MX names, differing only
/// in which IP is compared. Without memoization a server with four sending
/// addresses issues the same queries four times over.
///
/// The cache deliberately lives for one request rather than in `ApiState`. A
/// health check exists to report what DNS says *now*, so caching across
/// requests would answer from a stale view with no TTL to bound it. Wrapping
/// per request keeps the saving without that risk.
pub struct MemoizingSpfResolver<'a> {
    inner: &'a dyn camelmailer_core::SpfResolver,
    txt: tokio::sync::Mutex<std::collections::HashMap<String, Result<Vec<String>, DnsError>>>,
    addresses: tokio::sync::Mutex<
        std::collections::HashMap<String, Result<Vec<std::net::IpAddr>, DnsError>>,
    >,
    mx: tokio::sync::Mutex<std::collections::HashMap<String, Result<Vec<String>, DnsError>>>,
}

impl<'a> MemoizingSpfResolver<'a> {
    pub fn new(inner: &'a dyn camelmailer_core::SpfResolver) -> Self {
        Self {
            inner,
            txt: Default::default(),
            addresses: Default::default(),
            mx: Default::default(),
        }
    }
}

/// DNS names are case-insensitive, so fold before using one as a cache key.
fn cache_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

#[async_trait]
impl camelmailer_core::SpfResolver for MemoizingSpfResolver<'_> {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        let key = cache_key(name);
        let mut cache = self.txt.lock().await;
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
        let value = self.inner.txt(name).await;
        cache.insert(key, value.clone());
        value
    }

    async fn ip_addresses(&self, name: &str) -> Result<Vec<std::net::IpAddr>, DnsError> {
        let key = cache_key(name);
        let mut cache = self.addresses.lock().await;
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
        let value = self.inner.ip_addresses(name).await;
        cache.insert(key, value.clone());
        value
    }

    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError> {
        let key = cache_key(name);
        let mut cache = self.mx.lock().await;
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
        let value = self.inner.mx_hosts(name).await;
        cache.insert(key, value.clone());
        value
    }
}

/// The production [`crate::dns_checks::DnsCheckResolver`] for the
/// infrastructure dashboard's DNS checks: one resolver, reused for a whole
/// check run, with every lookup bounded by [`DNS_CHECK_TIMEOUT`] so a dead
/// name server cannot hold the request.
pub struct HickoryDnsCheckResolver {
    spf: HickorySpfResolver,
}

/// How long one lookup of the DNS checks may take.
pub const DNS_CHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl HickoryDnsCheckResolver {
    /// Asks the public resolvers `servers` (`dns.check_resolvers`).
    pub fn new(servers: &[SocketAddr]) -> Self {
        Self {
            spf: HickorySpfResolver::new(servers),
        }
    }
}

impl Default for HickoryDnsCheckResolver {
    fn default() -> Self {
        Self::new(&default_check_resolvers())
    }
}

async fn bounded<T>(
    lookup: impl std::future::Future<Output = Result<T, DnsError>>,
) -> Result<T, DnsError> {
    tokio::time::timeout(DNS_CHECK_TIMEOUT, lookup)
        .await
        .unwrap_or_else(|_| Err(DnsError::Lookup("timed out".into())))
}

#[async_trait]
impl crate::dns_checks::DnsCheckResolver for HickoryDnsCheckResolver {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        use camelmailer_core::SpfResolver;
        bounded(self.spf.txt(name)).await
    }

    async fn ip_addresses(&self, name: &str) -> Result<Vec<std::net::IpAddr>, DnsError> {
        use camelmailer_core::SpfResolver;
        bounded(self.spf.ip_addresses(name)).await
    }

    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError> {
        use camelmailer_core::SpfResolver;
        bounded(self.spf.mx_hosts(name)).await
    }

    async fn ptr(&self, address: std::net::IpAddr) -> Result<Vec<String>, DnsError> {
        use hickory_resolver::proto::rr::Name;
        bounded(async {
            match self.spf.resolver.reverse_lookup(Name::from(address)).await {
                Ok(lookup) => Ok(lookup
                    .answers()
                    .iter()
                    .filter_map(|record| match &record.data {
                        RData::PTR(name) => {
                            Some(name.0.to_utf8().trim_end_matches('.').to_string())
                        }
                        _ => None,
                    })
                    .collect()),
                Err(error) => empty_on_no_records(error, Vec::new()),
            }
        })
        .await
    }
}
