//! Which mailbox provider receives a delivery ([`ProviderGroup`]).
//!
//! Consumer domains (gmail.com, outlook.com, yahoo.com, …) are known by
//! name. Any other domain is classified by its MX host, which tells Google
//! Workspace, Microsoft 365 and Yahoo-hosted domains apart from the rest.
//! MX answers are cached for an hour, and a lookup that fails or takes
//! longer than two seconds counts as `other` (not cached), so the
//! classification never holds a worker up for long.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use camelmailer_core::{ProviderGroup, ProviderGroups};

/// The most preferred MX host of a domain, if it has one.
pub type MxLookup =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Option<String>> + Send>> + Send + Sync>;

const CACHE_TTL: Duration = Duration::from_secs(3600);
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(2);
const CACHE_LIMIT: usize = 20_000;

pub struct ProviderClassifier {
    groups: ProviderGroups,
    lookup: MxLookup,
    cache: Mutex<HashMap<String, (ProviderGroup, Instant)>>,
}

impl Default for ProviderClassifier {
    fn default() -> Self {
        Self::with_lookup(Arc::new(|domain| Box::pin(lowest_mx(domain))))
    }
}

impl ProviderClassifier {
    /// A classifier that asks `lookup` for MX hosts (tests).
    pub fn with_lookup(lookup: MxLookup) -> Self {
        Self {
            groups: ProviderGroups::defaults(),
            lookup,
            cache: Mutex::new(HashMap::new()),
        }
    }

    /// Classify with `groups` instead of the built-in groups.
    pub fn with_groups(mut self, groups: ProviderGroups) -> Self {
        self.groups = groups;
        self.cache.lock().unwrap().clear();
        self
    }

    /// The groups of the `provider_groups` config, merged into the
    /// built-in ones.
    pub fn groups_from_config(
        config: &[camelmailer_config::ProviderGroupConfig],
    ) -> ProviderGroups {
        let rules: Vec<camelmailer_core::ProviderGroupRule> = config
            .iter()
            .filter_map(|group| {
                Some(camelmailer_core::ProviderGroupRule {
                    name: ProviderGroup::parse(&group.name)?,
                    domains: group.domains.clone(),
                    mx_patterns: group.mx_patterns.clone(),
                })
            })
            .collect();
        ProviderGroups::with_rules(&rules)
    }

    /// The groups this classifier uses.
    pub fn groups(&self) -> &ProviderGroups {
        &self.groups
    }

    /// The provider group of a recipient domain.
    pub async fn classify(&self, domain: &str) -> ProviderGroup {
        if let Some(group) = self.groups.from_recipient_domain(domain) {
            return group;
        }
        let key = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        if let Some((group, at)) = self.cache.lock().unwrap().get(&key).cloned() {
            if at.elapsed() < CACHE_TTL {
                return group;
            }
        }
        match tokio::time::timeout(LOOKUP_TIMEOUT, (self.lookup)(key.clone())).await {
            Ok(answer) => {
                let group = answer
                    .map(|host| self.groups.from_mx_host(&host))
                    .unwrap_or_else(ProviderGroup::other);
                let mut cache = self.cache.lock().unwrap();
                if cache.len() >= CACHE_LIMIT {
                    cache.clear();
                }
                cache.insert(key, (group.clone(), Instant::now()));
                group
            }
            Err(_) => ProviderGroup::other(),
        }
    }
}

async fn lowest_mx(domain: String) -> Option<String> {
    let domain = idna::domain_to_ascii(&domain).unwrap_or(domain);
    let resolver = hickory_resolver::TokioResolver::builder_tokio()
        .ok()?
        .build()
        .ok()?;
    let lookup = resolver.mx_lookup(format!("{domain}.")).await.ok()?;
    lookup
        .answers()
        .iter()
        .filter_map(|record| match &record.data {
            hickory_resolver::proto::rr::RData::MX(mx) => Some(mx),
            _ => None,
        })
        .min_by_key(|mx| mx.preference)
        .map(|mx| mx.exchange.to_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn custom_domains_are_classified_by_their_mx_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let classifier = ProviderClassifier::with_lookup(Arc::new(move |domain: String| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                match domain.as_str() {
                    "acme.example" => Some("acme-example.mail.protection.outlook.com.".into()),
                    "shop.example" => Some("aspmx.l.google.com.".into()),
                    _ => None,
                }
            })
        }));
        assert_eq!(
            classifier.classify("gmail.com").await,
            ProviderGroup::google()
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "consumer domains need no lookup"
        );
        assert_eq!(
            classifier.classify("Acme.Example").await,
            ProviderGroup::microsoft()
        );
        assert_eq!(
            classifier.classify("acme.example").await,
            ProviderGroup::microsoft()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "cached");
        assert_eq!(
            classifier.classify("shop.example").await,
            ProviderGroup::google()
        );
        assert_eq!(
            classifier.classify("none.example").await,
            ProviderGroup::other()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_lookup_counts_as_other_and_is_not_cached() {
        let classifier = ProviderClassifier::with_lookup(Arc::new(|_domain: String| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Some("aspmx.l.google.com".to_string())
            })
        }));
        assert_eq!(
            classifier.classify("slow.example").await,
            ProviderGroup::other()
        );
        assert!(classifier.cache.lock().unwrap().is_empty());
    }
}
