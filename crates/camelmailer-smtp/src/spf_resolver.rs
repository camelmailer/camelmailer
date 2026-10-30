//! The production [`SpfResolver`]: hickory-resolver on tokio. Mirrors the
//! `HickoryDnsResolver` used for domain verification, but exposes the A/AAAA
//! and MX lookups an SPF evaluation additionally needs. Tests drive the
//! evaluator with `camelmailer_core::spf::StaticSpfResolver` instead.

use async_trait::async_trait;
use camelmailer_core::dns::DnsError;
use camelmailer_core::SpfResolver;
use hickory_resolver::net::NetError;
use hickory_resolver::proto::rr::RData;
use hickory_resolver::TokioResolver;
use std::net::IpAddr;

pub struct HickorySpfResolver {
    resolver: TokioResolver,
}

impl HickorySpfResolver {
    /// Build a resolver from the system configuration, or `None` when that
    /// fails (SPF then stays disabled — it is best-effort and never blocks).
    pub fn from_system() -> Option<Self> {
        TokioResolver::builder_tokio()
            .and_then(|builder| builder.build())
            .ok()
            .map(|resolver| Self { resolver })
    }
}

/// A "name has no such records" answer is a normal empty result for SPF
/// (→ `none`/no-match), not a lookup failure (→ `temperror`).
fn empty_on_no_records<T>(error: NetError) -> Result<Vec<T>, DnsError> {
    if error.is_no_records_found() {
        Ok(Vec::new())
    } else {
        Err(DnsError::Lookup(error.to_string()))
    }
}

#[async_trait]
impl SpfResolver for HickorySpfResolver {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        match self.resolver.txt_lookup(name).await {
            Ok(lookup) => Ok(lookup
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    // A TXT record is one or more character-strings; SPF
                    // concatenates them with no separator.
                    RData::TXT(txt) => Some(
                        txt.txt_data
                            .iter()
                            .map(|chunk| String::from_utf8_lossy(chunk))
                            .collect::<String>(),
                    ),
                    _ => None,
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
                .answers()
                .iter()
                .filter_map(|record| match &record.data {
                    RData::MX(mx) => Some(mx.exchange.to_utf8().trim_end_matches('.').to_string()),
                    _ => None,
                })
                .collect()),
            Err(error) => empty_on_no_records(error),
        }
    }
}
