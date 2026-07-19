//! The production [`SpfResolver`]: hickory-resolver on tokio. Mirrors the
//! `HickoryDnsResolver` used for domain verification, but exposes the A/AAAA
//! and MX lookups an SPF evaluation additionally needs. Tests drive the
//! evaluator with `camelmailer_core::spf::StaticSpfResolver` instead.

use async_trait::async_trait;
use camelmailer_core::dns::DnsError;
use camelmailer_core::SpfResolver;
use hickory_resolver::error::{ResolveError, ResolveErrorKind};
use hickory_resolver::TokioAsyncResolver;
use std::net::IpAddr;

pub struct HickorySpfResolver {
    resolver: TokioAsyncResolver,
}

impl HickorySpfResolver {
    /// Build a resolver from the system configuration, or `None` when that
    /// fails (SPF then stays disabled — it is best-effort and never blocks).
    pub fn from_system() -> Option<Self> {
        TokioAsyncResolver::tokio_from_system_conf()
            .ok()
            .map(|resolver| Self { resolver })
    }
}

/// A "name has no such records" answer is a normal empty result for SPF
/// (→ `none`/no-match), not a lookup failure (→ `temperror`).
fn empty_on_no_records<T>(error: ResolveError) -> Result<Vec<T>, DnsError> {
    match error.kind() {
        ResolveErrorKind::NoRecordsFound { .. } => Ok(Vec::new()),
        other => Err(DnsError::Lookup(other.to_string())),
    }
}

#[async_trait]
impl SpfResolver for HickorySpfResolver {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        match self.resolver.txt_lookup(name).await {
            Ok(lookup) => Ok(lookup
                .iter()
                .map(|txt| {
                    // A TXT record is one or more character-strings; SPF
                    // concatenates them with no separator.
                    txt.txt_data()
                        .iter()
                        .map(|chunk| String::from_utf8_lossy(chunk))
                        .collect::<String>()
                })
                .collect()),
            Err(error) => empty_on_no_records(error),
        }
    }

    async fn ip_addresses(&self, name: &str) -> Result<Vec<IpAddr>, DnsError> {
        match self.resolver.lookup_ip(name).await {
            Ok(lookup) => Ok(lookup.iter().collect()),
            Err(error) => empty_on_no_records(error),
        }
    }

    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError> {
        match self.resolver.mx_lookup(name).await {
            Ok(lookup) => Ok(lookup
                .iter()
                .map(|mx| mx.exchange().to_utf8().trim_end_matches('.').to_string())
                .collect()),
            Err(error) => empty_on_no_records(error),
        }
    }
}
