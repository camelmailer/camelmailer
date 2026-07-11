//! DMARC monitoring primitives shared by the API and the worker:
//! the parsed read models of stored aggregate reports, the TXT-record
//! analysis behind the domain health check, and the pure aggregation
//! that turns report rows into the compliance summary.
//!
//! The aggregate-report *XML* parser lives in `camelmailer-worker`
//! (it needs the decompression crates); everything here is plain
//! string/number work so both storage impls and the router tests share
//! one behaviour.

use crate::model::Id;
use chrono::{DateTime, Utc};
use std::collections::BTreeMap;

/// The special inbound-route target that makes the worker ingest a
/// message as a DMARC aggregate report instead of POSTing it anywhere.
/// Route validation accepts exactly this value besides http(s) URLs.
pub const DMARC_REPORTS_ENDPOINT: &str = "internal://dmarc-reports";

// ------------------------------------------------------------ storage model

/// A stored DMARC aggregate report (header data; rows live in
/// [`DmarcRecordRow`]). Tenant-scoped like messages: RLS in Postgres.
#[derive(Debug, Clone, PartialEq)]
pub struct DmarcReport {
    pub id: i64,
    pub server_id: Id,
    /// The domain the report is about (`policy_published/domain`).
    pub domain: String,
    /// Reporting organization (`report_metadata/org_name`).
    pub org_name: Option<String>,
    /// Reporting organization contact (`report_metadata/email`).
    pub org_email: Option<String>,
    /// The reporter's report id (`report_metadata/report_id`).
    pub report_id: String,
    pub date_range_begin: DateTime<Utc>,
    pub date_range_end: DateTime<Utc>,
    pub received_at: DateTime<Utc>,
    /// Number of rows the report carries (denormalized for lists).
    pub record_count: i64,
}

/// One `<record>` row of an aggregate report.
#[derive(Debug, Clone, PartialEq)]
pub struct DmarcRecordRow {
    pub id: i64,
    pub report_id: i64,
    pub source_ip: String,
    pub count: i64,
    /// `policy_evaluated/disposition`: none | quarantine | reject.
    pub disposition: String,
    /// Raw DKIM auth result (`auth_results/dkim/result`), when present.
    pub dkim_result: Option<String>,
    /// Raw SPF auth result (`auth_results/spf/result`), when present.
    pub spf_result: Option<String>,
    /// `policy_evaluated/dkim` == pass (DKIM aligned in the DMARC sense).
    pub dkim_aligned: bool,
    /// `policy_evaluated/spf` == pass (SPF aligned in the DMARC sense).
    pub spf_aligned: bool,
    pub header_from: Option<String>,
    pub envelope_from: Option<String>,
}

/// A report plus its rows, as parsed by the worker and handed to the
/// store in one transaction.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDmarcReport {
    pub server_id: Id,
    pub domain: String,
    pub org_name: Option<String>,
    pub org_email: Option<String>,
    pub report_id: String,
    pub date_range_begin: DateTime<Utc>,
    pub date_range_end: DateTime<Utc>,
    pub records: Vec<NewDmarcRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewDmarcRecord {
    pub source_ip: String,
    pub count: i64,
    pub disposition: String,
    pub dkim_result: Option<String>,
    pub spf_result: Option<String>,
    pub dkim_aligned: bool,
    pub spf_aligned: bool,
    pub header_from: Option<String>,
    pub envelope_from: Option<String>,
}

/// Narrowing for report/record queries: an optional domain and an
/// optional time window matched against the report's date range
/// (a report matches when its range overlaps `[from, to]`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DmarcFilter {
    pub domain: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
}

impl DmarcFilter {
    /// Does a report with this domain/date-range match the filter?
    pub fn matches(
        &self,
        domain: &str,
        range_begin: DateTime<Utc>,
        range_end: DateTime<Utc>,
    ) -> bool {
        if let Some(wanted) = &self.domain {
            if !wanted.eq_ignore_ascii_case(domain) {
                return false;
            }
        }
        if let Some(from) = self.from {
            if range_end < from {
                return false;
            }
        }
        if let Some(to) = self.to {
            if range_begin > to {
                return false;
            }
        }
        true
    }
}

// -------------------------------------------------------- compliance summary

/// Per-source aggregation (one sending IP as seen by the reporters).
#[derive(Debug, Clone, PartialEq)]
pub struct DmarcSourceStat {
    pub source_ip: String,
    /// Messages from this source (sum of row counts).
    pub count: i64,
    /// Percentage (0–100) of messages with SPF aligned.
    pub spf_aligned_pct: f64,
    /// Percentage (0–100) of messages with DKIM aligned.
    pub dkim_aligned_pct: f64,
    pub disposition_counts: BTreeMap<String, i64>,
}

/// The compliance summary of `GET /api/v2/server/dmarc/summary`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DmarcSummary {
    /// Total messages covered (sum of row counts).
    pub total: i64,
    /// Messages where DKIM **and** SPF were aligned.
    pub pass: i64,
    pub fail: i64,
    /// `pass / total` in 0.0–1.0 (0.0 when there is no data).
    pub pass_rate: f64,
    /// Top sources by volume (at most [`DMARC_SUMMARY_TOP_SOURCES`]).
    pub by_source: Vec<DmarcSourceStat>,
    pub by_disposition: BTreeMap<String, i64>,
}

/// How many sources the summary lists.
pub const DMARC_SUMMARY_TOP_SOURCES: usize = 20;

fn pct(part: i64, total: i64) -> f64 {
    if total == 0 {
        0.0
    } else {
        (part as f64 * 100.0 / total as f64 * 10.0).round() / 10.0
    }
}

/// Aggregate report rows into the compliance summary. Pure, so
/// `MemoryStore` and `PgStore` produce identical numbers from the same
/// rows. "pass" means both DKIM and SPF were aligned; sources are the
/// top senders by volume.
pub fn summarize(records: &[DmarcRecordRow]) -> DmarcSummary {
    let mut summary = DmarcSummary::default();
    struct Source {
        count: i64,
        spf_aligned: i64,
        dkim_aligned: i64,
        dispositions: BTreeMap<String, i64>,
    }
    let mut sources: BTreeMap<&str, Source> = BTreeMap::new();

    for record in records {
        summary.total += record.count;
        if record.dkim_aligned && record.spf_aligned {
            summary.pass += record.count;
        } else {
            summary.fail += record.count;
        }
        *summary
            .by_disposition
            .entry(record.disposition.clone())
            .or_insert(0) += record.count;

        let source = sources.entry(&record.source_ip).or_insert(Source {
            count: 0,
            spf_aligned: 0,
            dkim_aligned: 0,
            dispositions: BTreeMap::new(),
        });
        source.count += record.count;
        if record.spf_aligned {
            source.spf_aligned += record.count;
        }
        if record.dkim_aligned {
            source.dkim_aligned += record.count;
        }
        *source
            .dispositions
            .entry(record.disposition.clone())
            .or_insert(0) += record.count;
    }

    summary.pass_rate = if summary.total == 0 {
        0.0
    } else {
        (summary.pass as f64 / summary.total as f64 * 1000.0).round() / 1000.0
    };

    let mut by_source: Vec<DmarcSourceStat> = sources
        .into_iter()
        .map(|(ip, source)| DmarcSourceStat {
            source_ip: ip.to_string(),
            count: source.count,
            spf_aligned_pct: pct(source.spf_aligned, source.count),
            dkim_aligned_pct: pct(source.dkim_aligned, source.count),
            disposition_counts: source.dispositions,
        })
        .collect();
    // highest volume first; the BTreeMap origin makes ties deterministic
    by_source.sort_by(|a, b| b.count.cmp(&a.count).then(a.source_ip.cmp(&b.source_ip)));
    by_source.truncate(DMARC_SUMMARY_TOP_SOURCES);
    summary.by_source = by_source;
    summary
}

// ------------------------------------------------------ DNS record analysis

/// A parsed `_dmarc.<domain>` TXT record (RFC 7489 §6.3, the tags the
/// health check surfaces).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DmarcPolicy {
    /// `p=` — none | quarantine | reject (None when the tag is absent
    /// or carries an unknown value).
    pub p: Option<String>,
    /// `sp=` — subdomain policy.
    pub sp: Option<String>,
    /// `rua=` — aggregate report addresses, split on commas.
    pub rua: Vec<String>,
    /// `pct=` — sampling percentage (defaults to 100 when absent).
    pub pct: u8,
}

/// Is this TXT record a DMARC record (`v=DMARC1` first tag)?
pub fn is_dmarc_record(txt: &str) -> bool {
    txt.split(';')
        .next()
        .map(|tag| tag.trim().eq_ignore_ascii_case("v=DMARC1"))
        .unwrap_or(false)
}

/// Parse a DMARC TXT record into its policy tags. Returns `None` when
/// the record is not a DMARC record at all.
pub fn parse_dmarc_record(txt: &str) -> Option<DmarcPolicy> {
    if !is_dmarc_record(txt) {
        return None;
    }
    let mut policy = DmarcPolicy {
        pct: 100,
        ..Default::default()
    };
    for tag in txt.split(';') {
        let Some((name, value)) = tag.split_once('=') else {
            continue;
        };
        let (name, value) = (name.trim().to_ascii_lowercase(), value.trim());
        match name.as_str() {
            "p" | "sp" => {
                let value = value.to_ascii_lowercase();
                if matches!(value.as_str(), "none" | "quarantine" | "reject") {
                    if name == "p" {
                        policy.p = Some(value);
                    } else {
                        policy.sp = Some(value);
                    }
                }
            }
            "rua" => {
                policy.rua = value
                    .split(',')
                    .map(|address| address.trim().to_string())
                    .filter(|address| !address.is_empty())
                    .collect();
            }
            "pct" => {
                if let Ok(pct) = value.parse::<u8>() {
                    policy.pct = pct.min(100);
                }
            }
            _ => {}
        }
    }
    Some(policy)
}

/// Is this TXT record an SPF record (`v=spf1` first term)?
pub fn is_spf_record(txt: &str) -> bool {
    let first = txt.split_whitespace().next().unwrap_or("");
    first.eq_ignore_ascii_case("v=spf1")
}

/// The `all` qualifier of an SPF record: `-`, `~`, `?`, `+` — or `None`
/// when the record has no `all` term.
pub fn spf_all_qualifier(txt: &str) -> Option<char> {
    for term in txt.split_whitespace().skip(1) {
        let (qualifier, mechanism) = match term.chars().next() {
            Some(q @ ('-' | '~' | '?' | '+')) => (q, &term[1..]),
            _ => ('+', term),
        };
        if mechanism.eq_ignore_ascii_case("all") {
            return Some(qualifier);
        }
    }
    None
}

/// Does the SPF record contain the given mechanism (`include:x` /
/// `a:host` …), ignoring an optional leading qualifier and case?
pub fn spf_contains_mechanism(txt: &str, mechanism: &str) -> bool {
    txt.split_whitespace().skip(1).any(|term| {
        let term = term.trim_start_matches(['-', '~', '?', '+']);
        term.eq_ignore_ascii_case(mechanism)
    })
}

/// The `p=` value of a DKIM TXT record (concatenated), or `None` when
/// the record carries no `p=` tag.
pub fn dkim_public_key_of_record(txt: &str) -> Option<String> {
    for tag in txt.split(';') {
        if let Some((name, value)) = tag.split_once('=') {
            if name.trim().eq_ignore_ascii_case("p") {
                // whitespace inside the base64 comes from TXT chunking
                return Some(value.chars().filter(|c| !c.is_whitespace()).collect());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        source_ip: &str,
        count: i64,
        disposition: &str,
        dkim_aligned: bool,
        spf_aligned: bool,
    ) -> DmarcRecordRow {
        DmarcRecordRow {
            id: 0,
            report_id: 1,
            source_ip: source_ip.into(),
            count,
            disposition: disposition.into(),
            dkim_result: Some("pass".into()),
            spf_result: Some("pass".into()),
            dkim_aligned,
            spf_aligned,
            header_from: Some("example.com".into()),
            envelope_from: None,
        }
    }

    #[test]
    fn summarize_counts_pass_fail_and_dispositions() {
        let records = vec![
            row("1.1.1.1", 8, "none", true, true),
            row("1.1.1.1", 2, "none", true, false),
            row("2.2.2.2", 5, "quarantine", false, false),
        ];
        let summary = summarize(&records);
        assert_eq!(summary.total, 15);
        assert_eq!(summary.pass, 8);
        assert_eq!(summary.fail, 7);
        assert!((summary.pass_rate - 0.533).abs() < 1e-9);
        assert_eq!(summary.by_disposition["none"], 10);
        assert_eq!(summary.by_disposition["quarantine"], 5);

        assert_eq!(summary.by_source.len(), 2);
        // top source first
        assert_eq!(summary.by_source[0].source_ip, "1.1.1.1");
        assert_eq!(summary.by_source[0].count, 10);
        assert!((summary.by_source[0].dkim_aligned_pct - 100.0).abs() < 1e-9);
        assert!((summary.by_source[0].spf_aligned_pct - 80.0).abs() < 1e-9);
        assert_eq!(summary.by_source[1].disposition_counts["quarantine"], 5);
    }

    #[test]
    fn summarize_caps_sources_at_top_20() {
        let records: Vec<DmarcRecordRow> = (0..30)
            .map(|i| row(&format!("10.0.0.{i}"), i + 1, "none", true, true))
            .collect();
        let summary = summarize(&records);
        assert_eq!(summary.by_source.len(), DMARC_SUMMARY_TOP_SOURCES);
        // highest volume (10.0.0.29, count 30) leads
        assert_eq!(summary.by_source[0].count, 30);
    }

    #[test]
    fn summarize_of_nothing_is_zero() {
        let summary = summarize(&[]);
        assert_eq!(summary.total, 0);
        assert_eq!(summary.pass_rate, 0.0);
        assert!(summary.by_source.is_empty());
    }

    #[test]
    fn dmarc_records_are_detected_and_parsed() {
        assert!(is_dmarc_record("v=DMARC1; p=none"));
        assert!(is_dmarc_record("V=dmarc1;p=reject"));
        assert!(!is_dmarc_record("v=spf1 -all"));

        let policy = parse_dmarc_record(
            "v=DMARC1; p=quarantine; sp=reject; pct=50; rua=mailto:a@x.com, mailto:b@y.com",
        )
        .unwrap();
        assert_eq!(policy.p.as_deref(), Some("quarantine"));
        assert_eq!(policy.sp.as_deref(), Some("reject"));
        assert_eq!(policy.pct, 50);
        assert_eq!(policy.rua, vec!["mailto:a@x.com", "mailto:b@y.com"]);

        // unknown p values stay None; pct defaults to 100
        let policy = parse_dmarc_record("v=DMARC1; p=blocked").unwrap();
        assert_eq!(policy.p, None);
        assert_eq!(policy.pct, 100);

        assert!(parse_dmarc_record("v=spf1 -all").is_none());
    }

    #[test]
    fn spf_analysis_handles_qualifiers_and_mechanisms() {
        assert!(is_spf_record("v=spf1 include:x -all"));
        assert!(!is_spf_record("v=DMARC1; p=none"));

        assert_eq!(spf_all_qualifier("v=spf1 include:x -all"), Some('-'));
        assert_eq!(spf_all_qualifier("v=spf1 include:x ~all"), Some('~'));
        assert_eq!(spf_all_qualifier("v=spf1 include:x ?all"), Some('?'));
        assert_eq!(spf_all_qualifier("v=spf1 include:x all"), Some('+'));
        assert_eq!(spf_all_qualifier("v=spf1 include:x"), None);

        assert!(spf_contains_mechanism(
            "v=spf1 a include:spf.example.com ~all",
            "include:spf.example.com"
        ));
        assert!(spf_contains_mechanism(
            "v=spf1 +A:Mail.Example.Com -all",
            "a:mail.example.com"
        ));
        assert!(!spf_contains_mechanism("v=spf1 -all", "include:x"));
    }

    #[test]
    fn dkim_public_key_is_extracted_and_dechunked() {
        assert_eq!(
            dkim_public_key_of_record("v=DKIM1; k=rsa; p=MIIB AAAA").as_deref(),
            Some("MIIBAAAA")
        );
        assert_eq!(dkim_public_key_of_record("v=DKIM1; k=rsa"), None);
    }

    #[test]
    fn filter_matches_domain_and_overlapping_ranges() {
        let begin = chrono::Utc::now();
        let end = begin + chrono::Duration::days(1);
        let filter = DmarcFilter {
            domain: Some("Example.com".into()),
            from: Some(begin - chrono::Duration::hours(1)),
            to: Some(end + chrono::Duration::hours(1)),
        };
        assert!(filter.matches("example.com", begin, end));
        assert!(!filter.matches("other.com", begin, end));

        let after = DmarcFilter {
            from: Some(end + chrono::Duration::hours(1)),
            ..Default::default()
        };
        assert!(!after.matches("example.com", begin, end));

        let before = DmarcFilter {
            to: Some(begin - chrono::Duration::hours(1)),
            ..Default::default()
        };
        assert!(!before.matches("example.com", begin, end));
    }
}
