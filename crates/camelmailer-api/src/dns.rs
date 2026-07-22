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
