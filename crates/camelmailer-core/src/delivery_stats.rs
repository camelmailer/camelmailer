//! Delivery counters per sending IP and receiving provider.
//!
//! Every outbound SMTP result is counted per UTC day, per source address
//! and per receiving provider group (Microsoft, Google, Yahoo, others).
//! The counters feed the IP metrics of the infrastructure dashboard and
//! the adaptive warm-up, which judges an address per provider: an IP can be
//! warm at one mailbox provider and still cold at another.
//!
//! Counting never decides anything about a delivery. It runs after the
//! result is recorded, and a failure to count is logged and ignored.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::{Id, StoreError};

/// The source key of a delivery from a server without an IP pool: the
/// operating system picked the local address.
pub const DEFAULT_SOURCE: &str = "default";

/// The receiving mailbox provider group of a delivery, as far as
/// reputation goes. The large providers keep their own reputation per
/// sending IP, so each group is counted and warmed up separately; whoever
/// matches no group is `other`.
///
/// Groups are names (`[a-z0-9_]`, at most 40 characters). The defaults are
/// [`ProviderGroups::defaults`]; an installation adds or changes groups in
/// the `provider_groups` config without a code change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProviderGroup(String);

impl ProviderGroup {
    pub const OTHER: &'static str = "other";

    /// A group name, if it is valid.
    pub fn parse(value: &str) -> Option<ProviderGroup> {
        let valid = !value.is_empty()
            && value.len() <= 40
            && value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
        valid.then(|| ProviderGroup(value.to_string()))
    }

    /// A group name known to be valid (the built-in names).
    fn known(value: &str) -> ProviderGroup {
        ProviderGroup(value.to_string())
    }

    pub fn other() -> ProviderGroup {
        Self::known(Self::OTHER)
    }
    pub fn microsoft() -> ProviderGroup {
        Self::known("microsoft")
    }
    pub fn google() -> ProviderGroup {
        Self::known("google")
    }
    pub fn yahoo() -> ProviderGroup {
        Self::known("yahoo")
    }
    pub fn apple() -> ProviderGroup {
        Self::known("apple")
    }
    pub fn united_internet() -> ProviderGroup {
        Self::known("united_internet")
    }
    pub fn t_online() -> ProviderGroup {
        Self::known("t_online")
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_other(&self) -> bool {
        self.0 == Self::OTHER
    }
}

impl std::fmt::Display for ProviderGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for ProviderGroup {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        ProviderGroup::parse(&value).ok_or_else(|| format!("invalid provider group name {value:?}"))
    }
}

impl From<ProviderGroup> for String {
    fn from(group: ProviderGroup) -> String {
        group.0
    }
}

/// How one provider group is recognised.
///
/// `domains` are recipient domains of the provider's own mail service:
/// `gmail.com` matches that domain exactly, `hotmail.*` the brand followed
/// by any top-level domain (`hotmail.de`, `hotmail.co.uk`). `mx_patterns`
/// are MX host names: `google.com` matches `google.com` and every host
/// under it (`aspmx.l.google.com`), so a company domain hosted by the
/// provider (Google Workspace, Microsoft 365, IONOS) counts towards it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderGroupRule {
    pub name: ProviderGroup,
    #[serde(default)]
    pub domains: Vec<String>,
    #[serde(default)]
    pub mx_patterns: Vec<String>,
}

impl ProviderGroupRule {
    fn new(name: ProviderGroup, domains: &[&str], mx_patterns: &[&str]) -> Self {
        ProviderGroupRule {
            name,
            domains: domains.iter().map(|d| d.to_string()).collect(),
            mx_patterns: mx_patterns.iter().map(|m| m.to_string()).collect(),
        }
    }

    fn matches_domain(&self, domain: &str, labels: &[&str]) -> bool {
        // brand.* : the brand label followed by a top-level domain of one
        // label (hotmail.de) or a country form of two (hotmail.co.uk).
        let country_suffix = labels.len() == 2
            || (labels.len() == 3
                && matches!(labels[1], "co" | "com" | "net" | "org" | "ne" | "or"));
        self.domains.iter().any(|pattern| {
            let pattern = pattern.trim().trim_end_matches('.').to_ascii_lowercase();
            match pattern.strip_suffix(".*") {
                Some(brand) => country_suffix && labels[0] == brand,
                None => pattern == domain,
            }
        })
    }

    fn matches_mx(&self, host: &str) -> bool {
        self.mx_patterns.iter().any(|pattern| {
            let pattern = pattern
                .trim()
                .trim_start_matches("*.")
                .trim_end_matches('.')
                .to_ascii_lowercase();
            !pattern.is_empty() && (host == pattern || host.ends_with(&format!(".{pattern}")))
        })
    }
}

/// The provider groups of an installation, in the order they are tried.
/// Whatever matches none is [`ProviderGroup::other`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderGroups {
    rules: Vec<ProviderGroupRule>,
}

impl Default for ProviderGroups {
    fn default() -> Self {
        Self::defaults()
    }
}

impl ProviderGroups {
    /// The built-in groups: Microsoft (Outlook.com, Hotmail, Live, Microsoft
    /// 365), Google (Gmail, Workspace), Yahoo (incl. AOL), Apple (iCloud),
    /// United Internet (GMX, WEB.DE, mail.com, 1&1 / IONOS) and T-Online.
    pub fn defaults() -> Self {
        ProviderGroups {
            rules: vec![
                ProviderGroupRule::new(
                    ProviderGroup::microsoft(),
                    &[
                        "outlook.com",
                        "msn.com",
                        "passport.com",
                        "windowslive.com",
                        "hotmail.*",
                        "live.*",
                        "outlook.*",
                    ],
                    // *.mail.protection.outlook.com (Microsoft 365) and
                    // *.olc.protection.outlook.com (Outlook.com).
                    &["outlook.com", "hotmail.com", "microsoft.com"],
                ),
                ProviderGroupRule::new(
                    ProviderGroup::google(),
                    &["gmail.com", "googlemail.*"],
                    &["google.com", "googlemail.com"],
                ),
                ProviderGroupRule::new(
                    ProviderGroup::yahoo(),
                    &["yahoo.*", "ymail.com", "rocketmail.com", "aol.*", "aim.com"],
                    &["yahoodns.net", "yahoo.com", "aol.com"],
                ),
                ProviderGroupRule::new(
                    ProviderGroup::apple(),
                    &["icloud.com", "me.com", "mac.com"],
                    &["icloud.com", "apple.com"],
                ),
                ProviderGroupRule::new(
                    ProviderGroup::united_internet(),
                    &[
                        "gmx.*",
                        "web.de",
                        "mail.com",
                        "online.de",
                        "onlinehome.de",
                        "1und1.de",
                    ],
                    &[
                        "gmx.net",
                        "gmx.com",
                        "web.de",
                        "mail.com",
                        "kundenserver.de",
                        "ionos.de",
                        "ionos.com",
                    ],
                ),
                ProviderGroupRule::new(
                    ProviderGroup::t_online(),
                    &["t-online.de", "magenta.de"],
                    &["t-online.de"],
                ),
            ],
        }
    }

    /// The defaults with `rules` merged in: a rule with the name of a
    /// built-in group replaces it, any other is added (tried after the
    /// built-in groups). A rule named `other` is ignored: `other` is what
    /// matches nothing.
    pub fn with_rules(rules: &[ProviderGroupRule]) -> Self {
        let mut groups = Self::defaults();
        for rule in rules {
            if rule.name.is_other() {
                continue;
            }
            match groups.rules.iter_mut().find(|r| r.name == rule.name) {
                Some(existing) => *existing = rule.clone(),
                None => groups.rules.push(rule.clone()),
            }
        }
        groups
    }

    /// Every group name, `other` last.
    pub fn names(&self) -> Vec<ProviderGroup> {
        let mut names: Vec<ProviderGroup> = self.rules.iter().map(|r| r.name.clone()).collect();
        names.push(ProviderGroup::other());
        names
    }

    pub fn rules(&self) -> &[ProviderGroupRule] {
        &self.rules
    }

    /// The group of a recipient domain that belongs to a provider's own mail
    /// service. `None` for any other domain, whose provider only its MX
    /// records tell ([`ProviderGroups::from_mx_host`]).
    pub fn from_recipient_domain(&self, domain: &str) -> Option<ProviderGroup> {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let labels: Vec<&str> = domain.split('.').collect();
        if labels.len() < 2 {
            return None;
        }
        self.rules
            .iter()
            .find(|rule| rule.matches_domain(&domain, &labels))
            .map(|rule| rule.name.clone())
    }

    /// The group of the provider that runs an MX host.
    pub fn from_mx_host(&self, host: &str) -> ProviderGroup {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        self.rules
            .iter()
            .find(|rule| rule.matches_mx(&host))
            .map(|rule| rule.name.clone())
            .unwrap_or_else(ProviderGroup::other)
    }
}

/// How an outbound SMTP attempt ended, for counting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryKind {
    /// Accepted by the receiver.
    Sent,
    /// Temporary failure (4xx or a connection problem); the message waits.
    Deferred,
    /// Permanent failure (5xx, or attempts exhausted).
    Bounced,
}

/// Whether an SMTP reply is a reputation or policy refusal by the receiving
/// provider (as opposed to, say, an unknown mailbox). These are the replies
/// the warm-up brakes hard on. The markers are the documented ones of the
/// large providers: Microsoft's `S3150`/`S3140` blocks and `5.7.606`/
/// `5.7.511` banned-IP codes, Google's `4.7.28` (unusual rate of
/// unsolicited mail) and `5.7.1` policy refusals, Yahoo's `TSS04`/`TSS09`
/// deferrals, plus blocklist wording.
pub fn is_policy_block(response: &str) -> bool {
    const MARKERS: &[&str] = &[
        "4.7.28",
        "5.7.1 ",
        "5.7.1]",
        "5.7.1,",
        "5.7.606",
        "5.7.511",
        "s3150",
        "s3140",
        "tss04",
        "tss09",
        "blocked",
        "blacklist",
        "blocklist",
        "spamhaus",
        "listed at",
        "poor reputation",
        "bad reputation",
    ];
    let lower = response.to_ascii_lowercase();
    // "550-5.7.1 …" and "550 5.7.1 …" both carry the code followed by a
    // space or a dash-continued line; normalise the dash form.
    let lower = lower.replace("5.7.1-", "5.7.1 ");
    MARKERS.iter().any(|marker| lower.contains(marker)) || lower.ends_with("5.7.1")
}

/// One counted outbound attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpDeliveryRecord {
    /// The UTC day the attempt ended.
    pub day: NaiveDate,
    /// The pool address the delivery was bound to (its IPv4 value), or
    /// [`DEFAULT_SOURCE`].
    pub source_ip: String,
    /// The pool address row, when the delivery was bound to one.
    pub ip_address_id: Option<Id>,
    pub provider: ProviderGroup,
    pub kind: DeliveryKind,
    /// The reply was a reputation or policy refusal ([`is_policy_block`]).
    pub policy_block: bool,
    pub server_id: Id,
    pub message_id: i64,
}

/// The counters of one source address, provider and day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpDeliveryStat {
    pub day: NaiveDate,
    pub source_ip: String,
    pub ip_address_id: Option<Id>,
    pub provider: ProviderGroup,
    pub sent: i64,
    pub deferred: i64,
    pub bounced: i64,
    pub policy_blocks: i64,
    pub complaints: i64,
}

impl IpDeliveryStat {
    /// Every counted attempt (sent, deferred and bounced).
    pub fn attempts(&self) -> i64 {
        self.sent + self.deferred + self.bounced
    }
}

/// A complaint credited to the address that sent the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IpComplaint {
    pub source_ip: String,
    pub ip_address_id: Option<Id>,
    pub provider: ProviderGroup,
}

/// Storage of the delivery counters (`ip_delivery_stats`) and of which
/// address sent each accepted message (`message_sources`, kept briefly so a
/// spam complaint can be credited to the address that sent the message).
#[async_trait]
pub trait DeliveryStatsStore: Send + Sync {
    /// Count one attempt. An accepted message also remembers its source.
    async fn record_ip_delivery(&self, record: &IpDeliveryRecord) -> Result<(), StoreError>;

    /// Count a spam complaint against the address that sent `message_id`,
    /// on the day `at`. `None` when the source of the message is not known
    /// (sent before counting began, or past `message_sources` retention).
    async fn record_ip_complaint(
        &self,
        message_id: i64,
        at: DateTime<Utc>,
    ) -> Result<Option<IpComplaint>, StoreError>;

    /// The counters of every address and provider from `since` on, oldest
    /// day first.
    async fn ip_delivery_stats(&self, since: NaiveDate) -> Result<Vec<IpDeliveryStat>, StoreError>;

    /// Remove counters of days before `stats_before` and message sources
    /// recorded before `sources_before`. Returns how many rows it removed.
    async fn prune_delivery_stats(
        &self,
        stats_before: NaiveDate,
        sources_before: DateTime<Utc>,
    ) -> Result<u64, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumer_domains_map_to_their_provider() {
        let groups = ProviderGroups::defaults();
        for (domain, group) in [
            ("gmail.com", "google"),
            ("GoogleMail.com", "google"),
            ("outlook.com", "microsoft"),
            ("hotmail.de", "microsoft"),
            ("live.co.uk", "microsoft"),
            ("msn.com", "microsoft"),
            ("yahoo.com", "yahoo"),
            ("yahoo.co.jp", "yahoo"),
            ("aol.com", "yahoo"),
            ("aol.de", "yahoo"),
            ("ymail.com", "yahoo"),
            ("icloud.com", "apple"),
            ("me.com", "apple"),
            ("gmx.de", "united_internet"),
            ("gmx.net", "united_internet"),
            ("web.de", "united_internet"),
            ("mail.com", "united_internet"),
            ("t-online.de", "t_online"),
        ] {
            assert_eq!(
                groups.from_recipient_domain(domain).map(|g| g.to_string()),
                Some(group.to_string()),
                "{domain}"
            );
        }
        for domain in [
            "example.com",
            "live.example.com",
            "mail.yahoo.example.org",
            "com",
        ] {
            assert_eq!(groups.from_recipient_domain(domain), None, "{domain}");
        }
    }

    #[test]
    fn mx_hosts_map_to_the_provider_running_them() {
        let groups = ProviderGroups::defaults();
        for (host, group) in [
            ("aspmx.l.google.com.", "google"),
            ("alt1.gmr-smtp-in.l.google.com", "google"),
            ("example-com.mail.protection.outlook.com", "microsoft"),
            ("hotmail-com.olc.protection.outlook.com", "microsoft"),
            ("mta5.am0.yahoodns.net", "yahoo"),
            ("mx01.mail.icloud.com", "apple"),
            ("mx00.emig.gmx.net", "united_internet"),
            ("mx00.ionos.de", "united_internet"),
            ("mx01.kundenserver.de", "united_internet"),
            ("mx00.t-online.de", "t_online"),
            ("mx.example.net", "other"),
            ("notgoogle.com", "other"),
        ] {
            assert_eq!(groups.from_mx_host(host).as_str(), group, "{host}");
        }
    }

    #[test]
    fn configured_rules_replace_or_add_groups() {
        let groups = ProviderGroups::with_rules(&[
            ProviderGroupRule {
                name: ProviderGroup::parse("seznam").unwrap(),
                domains: vec!["seznam.cz".into(), "email.cz".into()],
                mx_patterns: vec!["*.seznam.cz".into()],
            },
            ProviderGroupRule {
                name: ProviderGroup::t_online(),
                domains: vec!["t-online.de".into()],
                mx_patterns: vec!["t-online.de".into(), "telekom.de".into()],
            },
            ProviderGroupRule {
                name: ProviderGroup::other(),
                domains: vec!["example.com".into()],
                mx_patterns: vec![],
            },
        ]);
        assert_eq!(
            groups.from_recipient_domain("email.cz").unwrap().as_str(),
            "seznam"
        );
        assert_eq!(groups.from_mx_host("mx1.seznam.cz").as_str(), "seznam");
        assert_eq!(groups.from_mx_host("mx.telekom.de").as_str(), "t_online");
        assert_eq!(groups.from_recipient_domain("magenta.de"), None, "replaced");
        assert_eq!(
            groups.from_recipient_domain("example.com"),
            None,
            "other is never a rule"
        );
        let names: Vec<String> = groups.names().iter().map(|g| g.to_string()).collect();
        assert_eq!(names.first().unwrap(), "microsoft");
        assert_eq!(names[names.len() - 2], "seznam");
        assert_eq!(names.last().unwrap(), "other");
    }

    #[test]
    fn provider_group_names_are_checked() {
        assert!(ProviderGroup::parse("united_internet").is_some());
        assert!(ProviderGroup::parse("").is_none());
        assert!(ProviderGroup::parse("Google").is_none());
        assert!(ProviderGroup::parse("a-b").is_none());
        assert!(ProviderGroup::parse(&"x".repeat(41)).is_none());
        let json = serde_json::to_string(&ProviderGroup::google()).unwrap();
        assert_eq!(json, "\"google\"");
        assert!(serde_json::from_str::<ProviderGroup>("\"No\"").is_err());
    }

    #[test]
    fn policy_blocks_are_told_apart_from_mailbox_errors() {
        for reply in [
            "550 5.7.1 Unfortunately, messages from [203.0.113.5] weren't sent. (S3150)",
            "421-4.7.28 Gmail has detected an unusual rate of unsolicited mail",
            "550-5.7.1 [203.0.113.5] Our system has detected that this message is likely unsolicited",
            "421 4.7.0 [TSS04] Messages from 203.0.113.5 temporarily deferred",
            "554 5.7.1 Service unavailable; Client host [203.0.113.5] blocked using zen.spamhaus.org",
            "550 5.7.606 Access denied, banned sending IP [203.0.113.5]",
        ] {
            assert!(is_policy_block(reply), "{reply}");
        }
        for reply in [
            "550 5.1.1 The email account that you tried to reach does not exist",
            "452 4.2.2 The email account that you tried to reach is over quota",
            "451 4.3.0 Temporary server error",
            "250 2.0.0 OK",
            "connection error to mx.example:25: timed out",
        ] {
            assert!(!is_policy_block(reply), "{reply}");
        }
    }
}

#[async_trait]
impl DeliveryStatsStore for crate::MemoryStore {
    async fn record_ip_delivery(&self, record: &IpDeliveryRecord) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let stat = inner
            .ip_delivery_stats
            .entry((
                record.day,
                record.source_ip.clone(),
                record.provider.clone(),
            ))
            .or_insert_with(|| IpDeliveryStat {
                day: record.day,
                source_ip: record.source_ip.clone(),
                ip_address_id: record.ip_address_id,
                provider: record.provider.clone(),
                sent: 0,
                deferred: 0,
                bounced: 0,
                policy_blocks: 0,
                complaints: 0,
            });
        stat.ip_address_id = record.ip_address_id.or(stat.ip_address_id);
        match record.kind {
            DeliveryKind::Sent => stat.sent += 1,
            DeliveryKind::Deferred => stat.deferred += 1,
            DeliveryKind::Bounced => stat.bounced += 1,
        }
        if record.policy_block {
            stat.policy_blocks += 1;
        }
        if record.kind == DeliveryKind::Sent {
            inner.message_sources.insert(
                record.message_id,
                (
                    IpComplaint {
                        source_ip: record.source_ip.clone(),
                        ip_address_id: record.ip_address_id,
                        provider: record.provider.clone(),
                    },
                    Utc::now(),
                ),
            );
        }
        Ok(())
    }

    async fn record_ip_complaint(
        &self,
        message_id: i64,
        at: DateTime<Utc>,
    ) -> Result<Option<IpComplaint>, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let Some((source, _)) = inner.message_sources.get(&message_id).cloned() else {
            return Ok(None);
        };
        let day = at.date_naive();
        let stat = inner
            .ip_delivery_stats
            .entry((day, source.source_ip.clone(), source.provider.clone()))
            .or_insert_with(|| IpDeliveryStat {
                day,
                source_ip: source.source_ip.clone(),
                ip_address_id: source.ip_address_id,
                provider: source.provider.clone(),
                sent: 0,
                deferred: 0,
                bounced: 0,
                policy_blocks: 0,
                complaints: 0,
            });
        stat.complaints += 1;
        Ok(Some(source))
    }

    async fn ip_delivery_stats(&self, since: NaiveDate) -> Result<Vec<IpDeliveryStat>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .ip_delivery_stats
            .values()
            .filter(|stat| stat.day >= since)
            .cloned()
            .collect())
    }

    async fn prune_delivery_stats(
        &self,
        stats_before: NaiveDate,
        sources_before: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let before = inner.ip_delivery_stats.len() + inner.message_sources.len();
        inner
            .ip_delivery_stats
            .retain(|(day, _, _), _| *day >= stats_before);
        inner
            .message_sources
            .retain(|_, (_, at)| *at >= sources_before);
        let after = inner.ip_delivery_stats.len() + inner.message_sources.len();
        Ok((before - after) as u64)
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;

    fn record(kind: DeliveryKind, message_id: i64) -> IpDeliveryRecord {
        IpDeliveryRecord {
            day: NaiveDate::from_ymd_opt(2026, 11, 14).unwrap(),
            source_ip: "203.0.113.5".into(),
            ip_address_id: Some(3),
            provider: ProviderGroup::microsoft(),
            kind,
            policy_block: false,
            server_id: 1,
            message_id,
        }
    }

    #[tokio::test]
    async fn attempts_are_counted_per_day_address_and_provider() {
        let store = crate::MemoryStore::new();
        store
            .record_ip_delivery(&record(DeliveryKind::Sent, 1))
            .await
            .unwrap();
        store
            .record_ip_delivery(&record(DeliveryKind::Sent, 2))
            .await
            .unwrap();
        let mut blocked = record(DeliveryKind::Bounced, 3);
        blocked.policy_block = true;
        store.record_ip_delivery(&blocked).await.unwrap();
        store
            .record_ip_delivery(&record(DeliveryKind::Deferred, 4))
            .await
            .unwrap();
        let mut google = record(DeliveryKind::Sent, 5);
        google.provider = ProviderGroup::google();
        store.record_ip_delivery(&google).await.unwrap();

        let at = DateTime::parse_from_rfc3339("2026-11-15T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let credited = store.record_ip_complaint(2, at).await.unwrap().unwrap();
        assert_eq!(credited.provider, ProviderGroup::microsoft());
        assert!(
            store.record_ip_complaint(3, at).await.unwrap().is_none(),
            "not accepted"
        );

        let stats = store
            .ip_delivery_stats(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap())
            .await
            .unwrap();
        assert_eq!(stats.len(), 3);
        let ms = stats
            .iter()
            .find(|s| s.provider == ProviderGroup::microsoft() && s.day.to_string() == "2026-11-14")
            .unwrap();
        assert_eq!(
            (ms.sent, ms.deferred, ms.bounced, ms.policy_blocks),
            (2, 1, 1, 1)
        );
        assert_eq!(ms.attempts(), 4);
        let complaint_day = stats
            .iter()
            .find(|s| s.day.to_string() == "2026-11-15")
            .unwrap();
        assert_eq!(complaint_day.complaints, 1);

        let removed = store
            .prune_delivery_stats(NaiveDate::from_ymd_opt(2026, 11, 15).unwrap(), at)
            .await
            .unwrap();
        assert_eq!(removed, 2 + 3, "two old counters and every source");
    }
}
