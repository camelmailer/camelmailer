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
use serde::Serialize;

use crate::{Id, StoreError};

/// The source key of a delivery from a server without an IP pool: the
/// operating system picked the local address.
pub const DEFAULT_SOURCE: &str = "default";

/// The receiving mailbox provider of a delivery, as far as reputation goes.
/// Microsoft, Google and Yahoo keep their own reputation per sending IP, so
/// each is judged separately; everybody else shares one bucket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderGroup {
    Microsoft,
    Google,
    Yahoo,
    Other,
}

impl ProviderGroup {
    pub const ALL: [ProviderGroup; 4] = [
        ProviderGroup::Microsoft,
        ProviderGroup::Google,
        ProviderGroup::Yahoo,
        ProviderGroup::Other,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ProviderGroup::Microsoft => "microsoft",
            ProviderGroup::Google => "google",
            ProviderGroup::Yahoo => "yahoo",
            ProviderGroup::Other => "other",
        }
    }

    pub fn parse(value: &str) -> Option<ProviderGroup> {
        match value {
            "microsoft" => Some(ProviderGroup::Microsoft),
            "google" => Some(ProviderGroup::Google),
            "yahoo" => Some(ProviderGroup::Yahoo),
            "other" => Some(ProviderGroup::Other),
            _ => None,
        }
    }

    /// The group of a recipient domain that belongs to a provider's own
    /// consumer service (gmail.com, outlook.com, hotmail.de, yahoo.co.uk,
    /// aol.com, …). `None` for any other domain, whose provider only its MX
    /// records tell ([`ProviderGroup::from_mx_host`]).
    pub fn from_recipient_domain(domain: &str) -> Option<ProviderGroup> {
        let domain = domain.trim().trim_end_matches('.').to_ascii_lowercase();
        let labels: Vec<&str> = domain.split('.').collect();
        if labels.len() < 2 {
            return None;
        }
        match domain.as_str() {
            "gmail.com" | "googlemail.com" => return Some(ProviderGroup::Google),
            "outlook.com" | "msn.com" | "passport.com" | "windowslive.com" => {
                return Some(ProviderGroup::Microsoft)
            }
            "ymail.com" | "rocketmail.com" | "aol.com" | "aim.com" | "aol.de" | "aol.co.uk"
            | "aol.fr" => return Some(ProviderGroup::Yahoo),
            _ => {}
        }
        // hotmail.de, live.co.uk, outlook.de, yahoo.fr: the brand label
        // followed by a top-level domain (one or two labels).
        let country_suffix = labels.len() == 2
            || (labels.len() == 3
                && matches!(labels[1], "co" | "com" | "net" | "org" | "ne" | "or"));
        if country_suffix {
            match labels[0] {
                "hotmail" | "live" | "outlook" => return Some(ProviderGroup::Microsoft),
                "yahoo" => return Some(ProviderGroup::Yahoo),
                "googlemail" => return Some(ProviderGroup::Google),
                _ => {}
            }
        }
        None
    }

    /// The group of the provider that runs an MX host: Google Workspace,
    /// Microsoft 365 / Exchange Online and Yahoo/AOL mail hosts.
    pub fn from_mx_host(host: &str) -> ProviderGroup {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        let under = |suffix: &str| host == suffix || host.ends_with(&format!(".{suffix}"));
        if under("google.com") || under("googlemail.com") {
            ProviderGroup::Google
        } else if under("outlook.com") || under("hotmail.com") || under("microsoft.com") {
            // *.mail.protection.outlook.com (Microsoft 365) and
            // *.olc.protection.outlook.com (Outlook.com) both end here.
            ProviderGroup::Microsoft
        } else if under("yahoodns.net") || under("yahoo.com") || under("aol.com") {
            ProviderGroup::Yahoo
        } else {
            ProviderGroup::Other
        }
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
        for (domain, group) in [
            ("gmail.com", ProviderGroup::Google),
            ("GoogleMail.com", ProviderGroup::Google),
            ("outlook.com", ProviderGroup::Microsoft),
            ("hotmail.de", ProviderGroup::Microsoft),
            ("live.co.uk", ProviderGroup::Microsoft),
            ("msn.com", ProviderGroup::Microsoft),
            ("yahoo.com", ProviderGroup::Yahoo),
            ("yahoo.co.jp", ProviderGroup::Yahoo),
            ("aol.com", ProviderGroup::Yahoo),
            ("ymail.com", ProviderGroup::Yahoo),
        ] {
            assert_eq!(
                ProviderGroup::from_recipient_domain(domain),
                Some(group),
                "{domain}"
            );
        }
        for domain in [
            "example.com",
            "live.example.com",
            "mail.yahoo.example.org",
            "com",
        ] {
            assert_eq!(
                ProviderGroup::from_recipient_domain(domain),
                None,
                "{domain}"
            );
        }
    }

    #[test]
    fn mx_hosts_map_to_the_provider_running_them() {
        for (host, group) in [
            ("aspmx.l.google.com.", ProviderGroup::Google),
            ("alt1.gmr-smtp-in.l.google.com", ProviderGroup::Google),
            (
                "example-com.mail.protection.outlook.com",
                ProviderGroup::Microsoft,
            ),
            (
                "hotmail-com.olc.protection.outlook.com",
                ProviderGroup::Microsoft,
            ),
            ("mta5.am0.yahoodns.net", ProviderGroup::Yahoo),
            ("mx.example.net", ProviderGroup::Other),
            ("notgoogle.com", ProviderGroup::Other),
        ] {
            assert_eq!(ProviderGroup::from_mx_host(host), group, "{host}");
        }
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

    #[test]
    fn provider_names_round_trip() {
        for group in ProviderGroup::ALL {
            assert_eq!(ProviderGroup::parse(group.as_str()), Some(group));
        }
        assert_eq!(ProviderGroup::parse("aol"), None);
    }
}

#[async_trait]
impl DeliveryStatsStore for crate::MemoryStore {
    async fn record_ip_delivery(&self, record: &IpDeliveryRecord) -> Result<(), StoreError> {
        let mut inner = self.inner.write().unwrap();
        let stat = inner
            .ip_delivery_stats
            .entry((record.day, record.source_ip.clone(), record.provider))
            .or_insert_with(|| IpDeliveryStat {
                day: record.day,
                source_ip: record.source_ip.clone(),
                ip_address_id: record.ip_address_id,
                provider: record.provider,
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
                        provider: record.provider,
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
            .entry((day, source.source_ip.clone(), source.provider))
            .or_insert_with(|| IpDeliveryStat {
                day,
                source_ip: source.source_ip.clone(),
                ip_address_id: source.ip_address_id,
                provider: source.provider,
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
            provider: ProviderGroup::Microsoft,
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
        google.provider = ProviderGroup::Google;
        store.record_ip_delivery(&google).await.unwrap();

        let at = DateTime::parse_from_rfc3339("2026-11-15T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let credited = store.record_ip_complaint(2, at).await.unwrap().unwrap();
        assert_eq!(credited.provider, ProviderGroup::Microsoft);
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
            .find(|s| s.provider == ProviderGroup::Microsoft && s.day.to_string() == "2026-11-14")
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
