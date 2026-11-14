//! DNS resolution behind a trait, so domain verification can be exercised
//! in tests without touching the network. The production implementation
//! (hickory-resolver on tokio) lives in `camelmailer-api`;
//! [`StaticDnsResolver`] is the in-memory test double.

use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

// Clone so a caller may memoize a lookup outcome (see the domain health
// check, which evaluates one SPF record once per sending address and would
// otherwise repeat every query). The variant holds only a message, so
// cloning copies a String and nothing else.
#[derive(Debug, Clone, thiserror::Error)]
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

/// Whether `name` is a fully qualified host name fit for an SMTP `EHLO`
/// (RFC 5321 4.1.1.1 / 2.3.5): at least two dot-separated labels of ASCII
/// letters, digits and inner hyphens, each 1 to 63 characters, 253 in all, a
/// top-level label that is not all digits (so no IP address), and no
/// trailing dot. Internationalised names must be given as A-labels
/// (`xn--…`). Everything else, CR, LF and spaces included, is rejected, so a
/// name that passes can be written into an SMTP command as it is.
pub fn is_valid_helo_hostname(name: &str) -> bool {
    if name.is_empty() || name.len() > 253 {
        return false;
    }
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let label_ok = |label: &&str| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    labels.iter().all(label_ok)
        && !labels
            .last()
            .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod helo_hostname_tests {
    use super::is_valid_helo_hostname;

    #[test]
    fn fully_qualified_names_pass() {
        for name in [
            "out1.example.com",
            "mx.camelmailer.test",
            "host-127.0.0.1.example",
            "xn--bcher-kva.example",
            "a.b",
        ] {
            assert!(is_valid_helo_hostname(name), "{name}");
        }
    }

    #[test]
    fn anything_that_could_break_or_misname_the_ehlo_line_fails() {
        let long_label = format!("{}.example", "a".repeat(64));
        let long_name = format!("{}.example", ["abcdefghi"; 26].join("."));
        for name in [
            "",
            "localhost",
            "out1",
            "out1.example.com.",
            ".example.com",
            "out1..example.com",
            "out1.example.com\r\nMAIL FROM:<x@evil>",
            "out1.example.com\n",
            "out1 .example.com",
            "out_1.example.com",
            "-out1.example.com",
            "out1-.example.com",
            "203.0.113.5",
            "[203.0.113.5]",
            "2001:db8::5",
            "bücher.example",
            long_label.as_str(),
            long_name.as_str(),
        ] {
            assert!(!is_valid_helo_hostname(name), "{name:?}");
        }
    }
}
