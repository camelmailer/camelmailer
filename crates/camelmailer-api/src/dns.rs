//! The production [`DnsResolver`]: hickory-resolver on tokio, using the
//! system resolver configuration (with the library defaults as fallback).
//! Used by `POST …/domains/{name}/verify`; tests inject
//! [`camelmailer_core::StaticDnsResolver`] instead.

use async_trait::async_trait;
use camelmailer_core::{DnsError, DnsResolver};
use hickory_resolver::config::{ResolverConfig, GOOGLE};
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::NetError;
use hickory_resolver::proto::rr::RData;
use hickory_resolver::TokioResolver;

/// A resolver from the system configuration, falling back to Google's public
/// resolvers (the hickory 0.24 `ResolverConfig::default()`) when the system
/// configuration cannot be read.
fn system_resolver() -> TokioResolver {
    TokioResolver::builder_tokio()
        .and_then(|builder| builder.build())
        .or_else(|_| {
            TokioResolver::builder_with_config(
                ResolverConfig::udp_and_tcp(&GOOGLE),
                TokioRuntimeProvider::default(),
            )
            .build()
        })
        .expect("a resolver with a static configuration always builds")
}

pub struct HickoryDnsResolver;

#[async_trait]
impl DnsResolver for HickoryDnsResolver {
    async fn txt_records(&self, name: &str) -> Result<Vec<String>, DnsError> {
        let resolver = system_resolver();
        match resolver.txt_lookup(format!("{name}.")).await {
            Ok(lookup) => Ok(txt_strings(&lookup)),
            // "no such record" is an answer, not a failure
            Err(error) if error.is_no_records_found() => Ok(Vec::new()),
            Err(error) => Err(DnsError::Lookup(error.to_string())),
        }
    }

    async fn cname(&self, name: &str) -> Result<Option<String>, DnsError> {
        use hickory_resolver::proto::rr::RecordType;
        let resolver = system_resolver();
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
    pub fn new() -> Self {
        let resolver = system_resolver();
        Self { resolver }
    }
}

impl Default for HickorySpfResolver {
    fn default() -> Self {
        Self::new()
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
