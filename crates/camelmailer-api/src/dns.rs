//! The production [`DnsResolver`]: hickory-resolver on tokio, using the
//! system resolver configuration (with the library defaults as fallback).
//! Used by `POST …/domains/{name}/verify`; tests inject
//! [`camelmailer_core::StaticDnsResolver`] instead.

use async_trait::async_trait;
use camelmailer_core::{DnsError, DnsResolver};
use hickory_resolver::config::{ResolverConfig, ResolverOpts};
use hickory_resolver::error::ResolveErrorKind;
use hickory_resolver::TokioAsyncResolver;

pub struct HickoryDnsResolver;

#[async_trait]
impl DnsResolver for HickoryDnsResolver {
    async fn txt_records(&self, name: &str) -> Result<Vec<String>, DnsError> {
        let resolver = TokioAsyncResolver::tokio_from_system_conf().unwrap_or_else(|_| {
            TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default())
        });
        match resolver.txt_lookup(format!("{name}.")).await {
            Ok(lookup) => Ok(lookup
                .iter()
                .map(|txt| {
                    // a TXT record may be split into several character
                    // strings; verifiers concatenate them
                    txt.iter()
                        .map(|data| String::from_utf8_lossy(data).into_owned())
                        .collect::<String>()
                })
                .collect()),
            Err(error) => match error.kind() {
                // "no such record" is an answer, not a failure
                ResolveErrorKind::NoRecordsFound { .. } => Ok(Vec::new()),
                _ => Err(DnsError::Lookup(error.to_string())),
            },
        }
    }

    async fn cname(&self, name: &str) -> Result<Option<String>, DnsError> {
        use hickory_resolver::proto::rr::{RData, RecordType};
        let resolver = TokioAsyncResolver::tokio_from_system_conf().unwrap_or_else(|_| {
            TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default())
        });
        match resolver.lookup(format!("{name}."), RecordType::CNAME).await {
            Ok(lookup) => Ok(lookup.iter().find_map(|rdata| match rdata {
                RData::CNAME(target) => {
                    Some(target.0.to_string().trim_end_matches('.').to_string())
                }
                _ => None,
            })),
            Err(error) => match error.kind() {
                // "no such record" is an answer, not a failure
                ResolveErrorKind::NoRecordsFound { .. } => Ok(None),
                _ => Err(DnsError::Lookup(error.to_string())),
            },
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
    resolver: TokioAsyncResolver,
}

impl HickorySpfResolver {
    pub fn new() -> Self {
        let resolver = TokioAsyncResolver::tokio_from_system_conf().unwrap_or_else(|_| {
            TokioAsyncResolver::tokio(ResolverConfig::default(), ResolverOpts::default())
        });
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
fn empty_on_no_records<T>(
    error: hickory_resolver::error::ResolveError,
    empty: T,
) -> Result<T, DnsError> {
    match error.kind() {
        ResolveErrorKind::NoRecordsFound { .. } => Ok(empty),
        _ => Err(DnsError::Lookup(error.to_string())),
    }
}

#[async_trait]
impl camelmailer_core::SpfResolver for HickorySpfResolver {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        match self.resolver.txt_lookup(format!("{name}.")).await {
            Ok(lookup) => Ok(lookup
                .iter()
                .map(|txt| {
                    txt.iter()
                        .map(|data| String::from_utf8_lossy(data).into_owned())
                        .collect::<String>()
                })
                .collect()),
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
                .iter()
                .map(|mx| mx.exchange().to_string().trim_end_matches('.').to_string())
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
