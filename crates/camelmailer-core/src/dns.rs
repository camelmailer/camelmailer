//! DNS resolution behind a trait, so domain verification can be exercised
//! in tests without touching the network. The production implementation
//! (hickory-resolver on tokio) lives in `camelmailer-api`;
//! [`StaticDnsResolver`] is the in-memory test double.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub enum DnsError {
    /// The lookup itself failed (network, resolver configuration, …) —
    /// distinct from "the name has no TXT records", which is `Ok(vec![])`.
    #[error("DNS lookup failed: {0}")]
    Lookup(String),
}

/// Asynchronous TXT-record resolution.
#[async_trait]
pub trait DnsResolver: Send + Sync {
    /// All TXT records published at `name` (empty when none exist).
    async fn txt_records(&self, name: &str) -> Result<Vec<String>, DnsError>;

    /// The CNAME target of `name`, without the trailing dot (`None` when the
    /// name has no CNAME record). Used by track-domain verification.
    async fn cname(&self, name: &str) -> Result<Option<String>, DnsError>;
}

/// In-memory resolver for tests: a name → TXT-values map plus an optional
/// forced lookup error.
#[derive(Default)]
pub struct StaticDnsResolver {
    records: RwLock<HashMap<String, Vec<String>>>,
    cnames: RwLock<HashMap<String, String>>,
    error: RwLock<Option<String>>,
}

impl StaticDnsResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a TXT record.
    pub fn add_txt(&self, name: &str, value: &str) {
        self.records
            .write()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .push(value.to_string());
    }

    /// Publish a CNAME record.
    pub fn add_cname(&self, name: &str, target: &str) {
        self.cnames
            .write()
            .unwrap()
            .insert(name.to_string(), target.to_string());
    }

    /// Make every subsequent lookup fail with `message`.
    pub fn fail_with(&self, message: &str) {
        *self.error.write().unwrap() = Some(message.to_string());
    }

    /// Let lookups succeed again after [`StaticDnsResolver::fail_with`].
    pub fn clear_error(&self) {
        *self.error.write().unwrap() = None;
    }
}

#[async_trait]
impl DnsResolver for StaticDnsResolver {
    async fn txt_records(&self, name: &str) -> Result<Vec<String>, DnsError> {
        if let Some(message) = self.error.read().unwrap().clone() {
            return Err(DnsError::Lookup(message));
        }
        Ok(self
            .records
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default())
    }

    async fn cname(&self, name: &str) -> Result<Option<String>, DnsError> {
        if let Some(message) = self.error.read().unwrap().clone() {
            return Err(DnsError::Lookup(message));
        }
        Ok(self.cnames.read().unwrap().get(name).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn static_resolver_returns_published_records_and_forced_errors() {
        let resolver = StaticDnsResolver::new();
        assert_eq!(
            resolver.txt_records("x.example").await.unwrap(),
            Vec::<String>::new()
        );
        resolver.add_txt("x.example", "one");
        resolver.add_txt("x.example", "two");
        assert_eq!(
            resolver.txt_records("x.example").await.unwrap(),
            vec!["one", "two"]
        );
        resolver.fail_with("SERVFAIL");
        assert!(matches!(
            resolver.txt_records("x.example").await,
            Err(DnsError::Lookup(message)) if message == "SERVFAIL"
        ));
    }
}
