//! Fetch provider feedback on a schedule: Google Postmaster Tools (API v2)
//! and Microsoft SNDS (automated data access). Complaint feedback loops
//! arrive as inbound ARF mail instead and are counted by the worker's ARF
//! ingestion.
//!
//! One worker fetches each source at a time: a fetch is claimed through the
//! `feedback_sources` row ([`FeedbackStore::claim_feedback_fetch`]), and a
//! failure moves the next attempt out exponentially (five minutes, doubled
//! per failure, at most the fetch interval). Snapshots are stored per day the
//! provider's data is about, so a refetch replaces rather than duplicates.
//!
//! Sources:
//!
//! - Postmaster Tools API v2, `POST /v2/{parent=domains/*}/domainStats:query`
//!   (https://developers.google.com/workspace/gmail/postmaster/reference/rest/v2/domains.domainStats/query),
//!   scope `https://www.googleapis.com/auth/postmaster.traffic.readonly`.
//!   Metrics from the v2 `StandardMetric` enum (discovery document
//!   https://gmailpostmastertools.googleapis.com/$discovery/rest?version=v2):
//!   `SPAM_RATE`, `AUTH_SUCCESS_RATE` (spf/dkim/dmarc), `TLS_ENCRYPTION_RATE`
//!   (outbound) and `DELIVERY_ERROR_RATE`. v2 has no domain or IP
//!   reputation; Google retired both
//!   (https://support.google.com/mail/answer/16594218).
//! - SNDS automated data access
//!   (https://substrate.office.com/ip-domain-management-snds/SNDS/FAQ): a
//!   URL carrying a key returns the same CSV as the portal's "Export to
//!   .CSV". The portal moved in 2026 from
//!   `sendersupport.olc.protection.outlook.com/snds` to
//!   `substrate.office.com/ip-domain-management-snds`; the old `data.aspx`
//!   URLs now redirect to the portal's home page, so a redirect or an HTML
//!   answer is reported as an outdated URL. Data for a day is aggregated
//!   after midnight Pacific time and kept for 90 days.

use std::sync::Arc;
use std::time::Duration;

use camelmailer_core::feedback::{provider, source, subject_kind, NewFeedbackSnapshot, Verdict};
use camelmailer_core::FeedbackStore;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{json, Value};

/// How often the scheduler looks for a due fetch.
const TICK: Duration = Duration::from_secs(60);
/// How long a claimed fetch may run before another worker may take over.
const LEASE_SECONDS: i64 = 600;
/// First retry delay after a failure; doubled per consecutive failure.
const FIRST_RETRY_SECONDS: i64 = 300;

/// Google's bulk sender guideline: keep the user-reported spam rate below
/// 0.1 % and never reach 0.3 %
/// (https://support.google.com/a/answer/81126).
pub const SPAM_RATE_WARN: f64 = 0.001;
pub const SPAM_RATE_BAD: f64 = 0.003;
/// SNDS: "more than 30% of the IPs ... keep their complaint rate at less
/// than 0.3% and this represents a good bar to shoot for" (SNDS FAQ).
pub const SNDS_COMPLAINT_WARN: f64 = 0.003;

// ------------------------------------------------------------ credentials

/// A secret from the environment variable `env`, else from the file at
/// `file`. Whitespace around it is trimmed. `None` when neither gives a
/// non-empty value; an unreadable file is reported in the error.
pub fn secret(
    env: &str,
    file: Option<&str>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    if let Some(value) = lookup(env).map(|v| v.trim().to_string()) {
        if !value.is_empty() {
            return Ok(Some(value));
        }
    }
    match file.map(str::trim).filter(|f| !f.is_empty()) {
        None => Ok(None),
        Some(path) => match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(value.trim().to_string()).filter(|v| !v.is_empty())),
            Err(error) => Err(format!("cannot read {path}: {error}")),
        },
    }
}

/// Postmaster Tools credentials, when all are present.
#[derive(Debug, Clone)]
pub struct PostmasterCredentials {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
}

/// Resolve the Postmaster Tools credentials. `Ok(None)`: not configured.
pub fn postmaster_credentials(
    config: &camelmailer_config::PostmasterTools,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<PostmasterCredentials>, String> {
    if config.domains.is_empty() {
        return Ok(None);
    }
    let client_id = lookup("CAMELMAILER_POSTMASTER_CLIENT_ID")
        .or_else(|| config.client_id.clone())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty());
    let client_secret = secret(
        "CAMELMAILER_POSTMASTER_CLIENT_SECRET",
        config.client_secret_file.as_deref(),
        lookup,
    )?;
    let refresh_token = secret(
        "CAMELMAILER_POSTMASTER_REFRESH_TOKEN",
        config.refresh_token_file.as_deref(),
        lookup,
    )?;
    Ok(match (client_id, client_secret, refresh_token) {
        (Some(client_id), Some(client_secret), Some(refresh_token)) => {
            Some(PostmasterCredentials {
                client_id,
                client_secret,
                refresh_token,
            })
        }
        _ => None,
    })
}

/// The SNDS data URL. `Ok(None)`: not configured.
pub fn snds_data_url(
    config: &camelmailer_config::Snds,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<String>, String> {
    secret(
        "CAMELMAILER_SNDS_DATA_URL",
        config.data_url_file.as_deref(),
        lookup,
    )
}

// ------------------------------------------------------- Postmaster Tools

/// The metrics asked for, by the name they come back under. Filters follow
/// the v2 `StandardMetric` descriptions.
const POSTMASTER_METRICS: [(&str, &str, &str); 9] = [
    ("spam_rate", "SPAM_RATE", ""),
    (
        "spf_success_rate",
        "AUTH_SUCCESS_RATE",
        r#"auth_type = "spf""#,
    ),
    (
        "dkim_success_rate",
        "AUTH_SUCCESS_RATE",
        r#"auth_type = "dkim""#,
    ),
    (
        "dmarc_success_rate",
        "AUTH_SUCCESS_RATE",
        r#"auth_type = "dmarc""#,
    ),
    (
        "tls_outbound_rate",
        "TLS_ENCRYPTION_RATE",
        r#"traffic_direction = "outbound""#,
    ),
    (
        "reject_rate",
        "DELIVERY_ERROR_RATE",
        r#"error_type = "reject""#,
    ),
    (
        "temp_fail_rate",
        "DELIVERY_ERROR_RATE",
        r#"error_type = "temp_fail""#,
    ),
    (
        "reject_low_ip_reputation_rate",
        "DELIVERY_ERROR_RATE",
        r#"error_type = "reject" AND error_reason = "low_ip_reputation""#,
    ),
    (
        "reject_low_domain_reputation_rate",
        "DELIVERY_ERROR_RATE",
        r#"error_type = "reject" AND error_reason = "low_domain_reputation""#,
    ),
];

fn google_date(date: NaiveDate) -> Value {
    use chrono::Datelike;
    json!({ "year": date.year(), "month": date.month(), "day": date.day() })
}

/// The `domainStats:query` request body for the `days` days up to
/// `today` (exclusive; today's data is never complete).
pub fn postmaster_query_body(today: NaiveDate, days: u32, page_token: Option<&str>) -> Value {
    let end = today - chrono::Duration::days(1);
    let start = today - chrono::Duration::days(i64::from(days.max(1)));
    let metrics: Vec<Value> = POSTMASTER_METRICS
        .iter()
        .map(|(name, metric, filter)| {
            let mut definition =
                json!({ "name": name, "baseMetric": { "standardMetric": metric } });
            if !filter.is_empty() {
                definition["filter"] = json!(filter);
            }
            definition
        })
        .collect();
    let mut body = json!({
        "metricDefinitions": metrics,
        "timeQuery": { "dateRanges": { "dateRanges": [
            { "start": google_date(start), "end": google_date(end) }
        ] } },
        "aggregationGranularity": "DAILY",
        "pageSize": 200,
    });
    if let Some(token) = page_token {
        body["pageToken"] = json!(token);
    }
    body
}

/// The number in a v2 `StatisticValue` (double, float, int64 as a string,
/// or a numeric string).
fn statistic_number(value: &Value) -> Option<f64> {
    for key in ["doubleValue", "floatValue"] {
        if let Some(number) = value.get(key).and_then(Value::as_f64) {
            return Some(number);
        }
    }
    for key in ["intValue", "stringValue"] {
        if let Some(number) = value
            .get(key)
            .and_then(|v| {
                v.as_str()
                    .map(str::to_string)
                    .or_else(|| v.as_f64().map(|n| n.to_string()))
            })
            .and_then(|s| s.trim().trim_end_matches('%').parse::<f64>().ok())
        {
            return Some(number);
        }
    }
    None
}

/// Collect one `QueryDomainStatsResponse` page into per-day metric maps.
pub fn collect_domain_stats(
    response: &Value,
    into: &mut std::collections::BTreeMap<NaiveDate, serde_json::Map<String, Value>>,
) {
    for stat in response
        .get("domainStats")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(metric), Some(date)) =
            (stat.get("metric").and_then(Value::as_str), stat.get("date"))
        else {
            continue;
        };
        let date = (|| {
            NaiveDate::from_ymd_opt(
                date.get("year")?.as_i64()? as i32,
                date.get("month")?.as_u64()? as u32,
                date.get("day")?.as_u64()? as u32,
            )
        })();
        let (Some(date), Some(number)) = (date, stat.get("value").and_then(statistic_number))
        else {
            continue;
        };
        into.entry(date)
            .or_default()
            .insert(metric.to_string(), json!(number));
    }
}

/// The verdict of one domain-day of Postmaster Tools data. Rates are
/// fractions (0.001 = 0.1 %).
pub fn postmaster_verdict(metrics: &serde_json::Map<String, Value>) -> (Verdict, String) {
    let number = |key: &str| metrics.get(key).and_then(Value::as_f64);
    let mut reasons = Vec::new();
    let mut verdict = Verdict::Unknown;
    if let Some(rate) = number("spam_rate") {
        let percent = rate * 100.0;
        if rate >= SPAM_RATE_BAD {
            verdict = verdict.max(Verdict::Bad);
            reasons.push(format!("spam rate {percent:.2} % (Google limit 0.3 %)"));
        } else if rate >= SPAM_RATE_WARN {
            verdict = verdict.max(Verdict::Warn);
            reasons.push(format!("spam rate {percent:.2} % (keep below 0.1 %)"));
        } else {
            verdict = verdict.max(Verdict::Ok);
            reasons.push(format!("spam rate {percent:.2} %"));
        }
    }
    for (key, label) in [
        (
            "reject_low_ip_reputation_rate",
            "rejected for low IP reputation",
        ),
        (
            "reject_low_domain_reputation_rate",
            "rejected for low domain reputation",
        ),
    ] {
        if let Some(rate) = number(key).filter(|rate| *rate > 0.0) {
            verdict = verdict.max(Verdict::Warn);
            reasons.push(format!("{:.2} % {label}", rate * 100.0));
        }
    }
    if verdict == Verdict::Unknown && !metrics.is_empty() {
        verdict = Verdict::Ok;
    }
    let detail = if reasons.is_empty() {
        "no spam rate reported for this day".to_string()
    } else {
        reasons.join("; ")
    };
    (verdict, detail)
}

/// Turn the per-day metrics of one domain into snapshots.
pub fn postmaster_snapshots(
    domain: &str,
    days: std::collections::BTreeMap<NaiveDate, serde_json::Map<String, Value>>,
) -> Vec<NewFeedbackSnapshot> {
    days.into_iter()
        .map(|(date, metrics)| {
            let (verdict, detail) = postmaster_verdict(&metrics);
            NewFeedbackSnapshot {
                source: source::POSTMASTER_TOOLS.into(),
                provider: provider::GOOGLE.into(),
                subject_kind: subject_kind::DOMAIN.into(),
                subject: domain.to_string(),
                data_date: date,
                verdict,
                detail,
                metrics: Value::Object(metrics),
            }
        })
        .collect()
}

async fn google_access_token(
    http: &reqwest::Client,
    token_url: &str,
    credentials: &PostmasterCredentials,
) -> Result<String, String> {
    let response = http
        .post(token_url)
        .form(&[
            ("client_id", credentials.client_id.as_str()),
            ("client_secret", credentials.client_secret.as_str()),
            ("refresh_token", credentials.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .await
        .map_err(|e| format!("token request failed: {e}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|e| format!("token response unreadable: {e}"))?;
    if !status.is_success() {
        // Never echo anything but Google's error code.
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return Err(format!("token request refused (HTTP {status}, {code})"));
    }
    body.get("access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "token response without access_token".to_string())
}

/// Fetch the last `config.days` days for every configured domain.
pub async fn fetch_postmaster(
    http: &reqwest::Client,
    config: &camelmailer_config::PostmasterTools,
    credentials: &PostmasterCredentials,
    today: NaiveDate,
) -> Result<Vec<NewFeedbackSnapshot>, String> {
    let token = google_access_token(http, &config.token_url, credentials).await?;
    let base = config.api_base_url.trim_end_matches('/');
    let mut snapshots = Vec::new();
    for domain in &config.domains {
        let domain = domain.trim().to_ascii_lowercase();
        let url = format!("{base}/v2/domains/{domain}/domainStats:query");
        let mut days = std::collections::BTreeMap::new();
        let mut page_token: Option<String> = None;
        for _ in 0..20 {
            let response = http
                .post(&url)
                .bearer_auth(&token)
                .json(&postmaster_query_body(
                    today,
                    config.days,
                    page_token.as_deref(),
                ))
                .send()
                .await
                .map_err(|e| format!("{domain}: request failed: {e}"))?;
            let status = response.status();
            let body: Value = response
                .json()
                .await
                .map_err(|e| format!("{domain}: response unreadable: {e}"))?;
            if !status.is_success() {
                let message = body
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                return Err(format!("{domain}: HTTP {status} {message}"));
            }
            collect_domain_stats(&body, &mut days);
            page_token = body
                .get("nextPageToken")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .map(str::to_string);
            if page_token.is_none() {
                break;
            }
        }
        snapshots.extend(postmaster_snapshots(&domain, days));
    }
    Ok(snapshots)
}

// ------------------------------------------------------------------ SNDS

/// One row of the SNDS export.
#[derive(Debug, Clone, PartialEq)]
pub struct SndsRow {
    pub ip: String,
    pub activity_end: Option<NaiveDate>,
    pub rcpt_commands: Option<i64>,
    pub data_commands: Option<i64>,
    pub recipients: Option<i64>,
    pub filter_result: String,
    /// As a fraction; `<0.1%` reads as 0.0 with `complaint_below` set.
    pub complaint_rate: Option<f64>,
    pub complaint_below: bool,
    pub trap_hits: Option<i64>,
    pub sample_helo: Option<String>,
    pub comments: Option<String>,
}

/// The column order of the SNDS CSV export (no header row): IP address,
/// activity start, activity end, RCPT commands, DATA commands, message
/// recipients, filter result, complaint rate, trap message start, trap
/// message end, trap hits, sample HELO, JMR P1 sender, comments. A header
/// row, if a future export adds one, is skipped.
pub fn parse_snds_csv(body: &str) -> Result<Vec<SndsRow>, String> {
    let trimmed = body.trim_start();
    if trimmed.starts_with('<') {
        return Err(
            "SNDS answered with an HTML page, not CSV: the data URL is outdated or its key \
             was revoked; copy the current URL from the SNDS Automated Access page"
                .into(),
        );
    }
    let mut rows = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        let Some(ip) = fields
            .first()
            .filter(|f| f.parse::<std::net::IpAddr>().is_ok())
        else {
            continue; // a header or a malformed line
        };
        let field = |i: usize| fields.get(i).copied().filter(|f| !f.is_empty());
        let number = |i: usize| field(i).and_then(|f| f.parse::<i64>().ok());
        let date = |i: usize| {
            field(i).and_then(|f| {
                let day = f.split_whitespace().next()?;
                NaiveDate::parse_from_str(day, "%m/%d/%Y")
                    .or_else(|_| NaiveDate::parse_from_str(day, "%Y-%m-%d"))
                    .ok()
            })
        };
        let complaint = field(7).unwrap_or("");
        let complaint_below = complaint.starts_with('<');
        let complaint_rate = complaint
            .trim_start_matches('<')
            .trim()
            .trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .ok()
            .map(|percent| {
                if complaint_below {
                    0.0
                } else {
                    percent / 100.0
                }
            });
        rows.push(SndsRow {
            ip: ip.to_string(),
            activity_end: date(2).or(date(1)),
            rcpt_commands: number(3),
            data_commands: number(4),
            recipients: number(5),
            filter_result: field(6).unwrap_or("").to_ascii_uppercase(),
            complaint_rate,
            complaint_below,
            trap_hits: number(10),
            sample_helo: field(11).map(str::to_string),
            comments: field(13).map(str::to_string),
        });
    }
    Ok(rows)
}

/// The verdict of one SNDS row: the filter result first (green = spam
/// verdicts below 10 %, yellow 10 to 90 %, red above 90 %), then the
/// complaint rate and trap hits.
pub fn snds_verdict(row: &SndsRow) -> (Verdict, String) {
    let mut reasons = Vec::new();
    let mut verdict = match row.filter_result.as_str() {
        "GREEN" => Verdict::Ok,
        "YELLOW" => Verdict::Warn,
        "RED" => Verdict::Bad,
        _ => Verdict::Unknown,
    };
    if !row.filter_result.is_empty() {
        reasons.push(format!(
            "filter result {}",
            row.filter_result.to_lowercase()
        ));
    }
    if let Some(rate) = row.complaint_rate {
        if row.complaint_below {
            reasons.push("complaint rate below 0.1 %".into());
        } else {
            reasons.push(format!("complaint rate {:.2} %", rate * 100.0));
            if rate >= SNDS_COMPLAINT_WARN {
                verdict = verdict.max(Verdict::Warn);
            }
        }
    }
    if let Some(hits) = row.trap_hits.filter(|hits| *hits > 0) {
        verdict = verdict.max(Verdict::Warn);
        reasons.push(format!("{hits} trap hits"));
    }
    (verdict, reasons.join("; "))
}

pub fn snds_snapshots(rows: Vec<SndsRow>, today: NaiveDate) -> Vec<NewFeedbackSnapshot> {
    rows.into_iter()
        .map(|row| {
            let (verdict, detail) = snds_verdict(&row);
            NewFeedbackSnapshot {
                source: source::SNDS.into(),
                provider: provider::MICROSOFT.into(),
                subject_kind: subject_kind::IP.into(),
                subject: row.ip.clone(),
                data_date: row
                    .activity_end
                    .unwrap_or(today - chrono::Duration::days(1)),
                verdict,
                detail,
                metrics: json!({
                    "rcpt_commands": row.rcpt_commands,
                    "data_commands": row.data_commands,
                    "recipients": row.recipients,
                    "filter_result": row.filter_result,
                    "complaint_rate": row.complaint_rate,
                    "complaint_rate_below_0_1_percent": row.complaint_below,
                    "trap_hits": row.trap_hits,
                    "sample_helo": row.sample_helo,
                    "comments": row.comments,
                }),
            }
        })
        .collect()
}

/// Fetch and parse the SNDS export. Redirects are not followed: a moved or
/// revoked URL must surface as an error instead of parsing a web page.
pub async fn fetch_snds(
    http: &reqwest::Client,
    data_url: &str,
    today: NaiveDate,
) -> Result<Vec<NewFeedbackSnapshot>, String> {
    let response = http
        .get(data_url)
        .send()
        .await
        // The URL carries the key: never put it in the error.
        .map_err(|e| format!("request failed: {}", e.without_url()))?;
    let status = response.status();
    if status.is_redirection() {
        return Err(format!(
            "HTTP {status}: the SNDS data URL redirects (the portal moved in 2026); copy the \
             current URL from the SNDS Automated Access page"
        ));
    }
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let body = response
        .text()
        .await
        .map_err(|e| format!("response unreadable: {}", e.without_url()))?;
    Ok(snds_snapshots(parse_snds_csv(&body)?, today))
}

// --------------------------------------------------------------- schedule

/// When to try again after `failures` consecutive failures.
pub fn next_attempt(now: DateTime<Utc>, interval_seconds: u32, failures: i32) -> DateTime<Utc> {
    let interval = i64::from(interval_seconds.max(60));
    let delay = if failures <= 0 {
        interval
    } else {
        let doubled =
            FIRST_RETRY_SECONDS.saturating_mul(1_i64 << failures.clamp(1, 20).saturating_sub(1));
        doubled.min(interval)
    };
    now + chrono::Duration::seconds(delay)
}

/// Reads a secret by environment variable name.
type SecretLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The fetch scheduler of one worker process.
pub struct FeedbackFetcher {
    config: camelmailer_config::DeliverabilityFeedback,
    store: Arc<dyn FeedbackStore>,
    http: reqwest::Client,
    holder: String,
    lookup: SecretLookup,
}

impl FeedbackFetcher {
    pub fn new(config: &camelmailer_config::Config, store: Arc<dyn FeedbackStore>) -> Self {
        Self {
            config: config.deliverability_feedback.clone(),
            store,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("reqwest client"),
            holder: format!("feedback-{}", camelmailer_core::token::generate_token(6)),
            lookup: Box::new(|name| std::env::var(name).ok()),
        }
    }

    /// Read secrets through `lookup` instead of the environment (tests).
    pub fn with_lookup(
        mut self,
        lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.lookup = Box::new(lookup);
        self
    }

    /// One scheduler round: record which sources are configured, then fetch
    /// every configured source that is due and not held by another worker.
    /// Returns the sources fetched (successfully or not).
    pub async fn run_once(&self) -> Vec<(&'static str, Result<usize, String>)> {
        let mut done = Vec::new();
        let today = Utc::now().date_naive();

        let postmaster = postmaster_credentials(&self.config.postmaster_tools, &*self.lookup);
        let snds = snds_data_url(&self.config.snds, &*self.lookup);
        let _ = self
            .store
            .set_feedback_source_enabled(source::FBL, true)
            .await;
        for (name, configured) in [
            (source::POSTMASTER_TOOLS, !matches!(postmaster, Ok(None))),
            (source::SNDS, !matches!(snds, Ok(None))),
        ] {
            if let Err(error) = self
                .store
                .set_feedback_source_enabled(name, configured)
                .await
            {
                tracing::warn!(%error, source = name, "could not record the feedback source state");
            }
        }

        if let Ok(snds) = &snds {
            if snds.is_none() && postmaster.as_ref().is_ok_and(Option::is_none) {
                return done;
            }
        }

        match postmaster {
            Ok(None) => {}
            Ok(Some(credentials)) => {
                if self.claim(source::POSTMASTER_TOOLS).await {
                    let result = fetch_postmaster(
                        &self.http,
                        &self.config.postmaster_tools,
                        &credentials,
                        today,
                    )
                    .await;
                    done.push((
                        source::POSTMASTER_TOOLS,
                        self.finish(source::POSTMASTER_TOOLS, result).await,
                    ));
                }
            }
            Err(error) => {
                if self.claim(source::POSTMASTER_TOOLS).await {
                    done.push((
                        source::POSTMASTER_TOOLS,
                        self.finish(source::POSTMASTER_TOOLS, Err(error)).await,
                    ));
                }
            }
        }
        match snds {
            Ok(None) => {}
            Ok(Some(url)) => {
                if self.claim(source::SNDS).await {
                    let result = fetch_snds(&self.http, &url, today).await;
                    done.push((source::SNDS, self.finish(source::SNDS, result).await));
                }
            }
            Err(error) => {
                if self.claim(source::SNDS).await {
                    done.push((source::SNDS, self.finish(source::SNDS, Err(error)).await));
                }
            }
        }
        done
    }

    async fn claim(&self, name: &str) -> bool {
        match self
            .store
            .claim_feedback_fetch(name, &self.holder, LEASE_SECONDS)
            .await
        {
            Ok(claimed) => claimed,
            Err(error) => {
                tracing::warn!(%error, source = name, "could not claim the feedback fetch");
                false
            }
        }
    }

    async fn finish(
        &self,
        name: &'static str,
        result: Result<Vec<NewFeedbackSnapshot>, String>,
    ) -> Result<usize, String> {
        let result = match result {
            Ok(snapshots) => self
                .store
                .upsert_feedback_snapshots(&snapshots)
                .await
                .map_err(|e| format!("could not store the snapshots: {e}")),
            Err(error) => Err(error),
        };
        let failures = match &result {
            Ok(_) => 0,
            Err(_) => self
                .store
                .feedback_source_statuses()
                .await
                .ok()
                .and_then(|statuses| {
                    statuses
                        .into_iter()
                        .find(|s| s.source == name)
                        .map(|s| s.consecutive_failures + 1)
                })
                .unwrap_or(1),
        };
        let next = next_attempt(Utc::now(), self.config.fetch_interval_seconds, failures);
        match &result {
            Ok(count) => tracing::info!(
                source = name,
                snapshots = count,
                "provider feedback fetched"
            ),
            Err(error) => {
                tracing::warn!(source = name, %error, retry_at = %next, "provider feedback fetch failed")
            }
        }
        if let Err(error) = self
            .store
            .finish_feedback_fetch(
                name,
                &self.holder,
                result.as_ref().err().map(String::as_str),
                next,
            )
            .await
        {
            tracing::warn!(%error, source = name, "could not record the feedback fetch result");
        }
        result
    }
}

/// Start the scheduler, unless `deliverability_feedback.fetch_interval_seconds`
/// is 0. Prunes old snapshots once a day.
pub fn spawn(
    config: &camelmailer_config::Config,
    store: Arc<dyn FeedbackStore>,
) -> Option<tokio::task::JoinHandle<()>> {
    if config.deliverability_feedback.fetch_interval_seconds == 0 {
        tracing::info!("scheduled provider feedback fetch is off");
        return None;
    }
    let retention = config.deliverability_feedback.snapshot_retention_days;
    let fetcher = FeedbackFetcher::new(config, store.clone());
    Some(tokio::spawn(async move {
        let mut last_prune: Option<std::time::Instant> = None;
        loop {
            fetcher.run_once().await;
            if retention > 0
                && last_prune.is_none_or(|at| at.elapsed() >= Duration::from_secs(86_400))
            {
                last_prune = Some(std::time::Instant::now());
                match store.prune_feedback_snapshots(retention).await {
                    Ok(0) => {}
                    Ok(removed) => tracing::info!(removed, "pruned provider feedback snapshots"),
                    Err(error) => {
                        tracing::warn!(%error, "could not prune provider feedback snapshots")
                    }
                }
            }
            tokio::time::sleep(TICK).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn the_postmaster_query_asks_for_complete_days_only() {
        let body = postmaster_query_body(day(2026, 11, 14), 7, None);
        let range = &body["timeQuery"]["dateRanges"]["dateRanges"][0];
        assert_eq!(range["start"], json!({"year": 2026, "month": 11, "day": 7}));
        assert_eq!(range["end"], json!({"year": 2026, "month": 11, "day": 13}));
        assert_eq!(body["aggregationGranularity"], "DAILY");
        let names: Vec<&str> = body["metricDefinitions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"spam_rate"));
        assert_eq!(
            body["metricDefinitions"][1]["filter"],
            json!(r#"auth_type = "spf""#)
        );
        let paged = postmaster_query_body(day(2026, 11, 14), 7, Some("abc"));
        assert_eq!(paged["pageToken"], "abc");
    }

    #[test]
    fn postmaster_responses_become_daily_snapshots_with_a_verdict() {
        let response: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/postmaster_domain_stats.json"
        ))
        .unwrap();
        let mut days = std::collections::BTreeMap::new();
        collect_domain_stats(&response, &mut days);
        let snapshots = postmaster_snapshots("example.com", days);
        assert_eq!(snapshots.len(), 3);
        let by_day = |d: u32| {
            snapshots
                .iter()
                .find(|s| s.data_date == day(2026, 11, d))
                .unwrap()
        };
        assert_eq!(by_day(11).verdict, Verdict::Ok);
        assert_eq!(by_day(11).metrics["dkim_success_rate"], json!(1.0));
        assert_eq!(by_day(12).verdict, Verdict::Warn);
        assert!(
            by_day(12).detail.contains("0.15 %"),
            "{}",
            by_day(12).detail
        );
        assert_eq!(by_day(13).verdict, Verdict::Bad);
        assert!(by_day(13).detail.contains("low IP reputation"));
        assert_eq!(by_day(13).subject_kind, "domain");
        assert_eq!(by_day(13).provider, "google");
    }

    #[test]
    fn snds_rows_parse_and_judge() {
        let rows = parse_snds_csv(include_str!("../tests/fixtures/snds_data.csv")).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].ip, "192.0.2.10");
        assert_eq!(rows[0].activity_end, Some(day(2026, 11, 12)));
        assert_eq!(rows[0].recipients, Some(1180));
        assert!(rows[0].complaint_below);
        assert_eq!(snds_verdict(&rows[0]).0, Verdict::Ok);
        assert_eq!(rows[1].complaint_rate, Some(0.005));
        let (verdict, detail) = snds_verdict(&rows[1]);
        assert_eq!(verdict, Verdict::Warn);
        assert!(detail.contains("complaint rate 0.50 %"), "{detail}");
        assert_eq!(snds_verdict(&rows[2]).0, Verdict::Bad);
        assert!(snds_verdict(&rows[2]).1.contains("3 trap hits"));
        let snapshots = snds_snapshots(rows, day(2026, 11, 14));
        assert_eq!(snapshots[2].subject, "198.51.100.7");
        assert_eq!(snapshots[2].subject_kind, "ip");
    }

    #[test]
    fn an_html_answer_from_snds_is_an_outdated_url() {
        let error = parse_snds_csv("<!DOCTYPE html><html>…").unwrap_err();
        assert!(error.contains("outdated"));
    }

    #[test]
    fn failures_back_off_up_to_the_interval() {
        let now = Utc::now();
        let secs = |at: DateTime<Utc>| (at - now).num_seconds();
        assert_eq!(secs(next_attempt(now, 21_600, 0)), 21_600);
        assert_eq!(secs(next_attempt(now, 21_600, 1)), 300);
        assert_eq!(secs(next_attempt(now, 21_600, 2)), 600);
        assert_eq!(secs(next_attempt(now, 21_600, 3)), 1_200);
        assert_eq!(secs(next_attempt(now, 21_600, 30)), 21_600);
    }

    #[test]
    fn missing_credentials_disable_a_source_and_env_wins_over_files() {
        let mut config = camelmailer_config::PostmasterTools::default();
        let none = |_: &str| None;
        assert!(postmaster_credentials(&config, &none).unwrap().is_none());
        config.domains = vec!["example.com".into()];
        assert!(postmaster_credentials(&config, &none).unwrap().is_none());
        let env = |name: &str| match name {
            "CAMELMAILER_POSTMASTER_CLIENT_ID" => Some("id".to_string()),
            "CAMELMAILER_POSTMASTER_CLIENT_SECRET" => Some("secret".to_string()),
            "CAMELMAILER_POSTMASTER_REFRESH_TOKEN" => Some(" token\n".to_string()),
            _ => None,
        };
        let credentials = postmaster_credentials(&config, &env).unwrap().unwrap();
        assert_eq!(credentials.refresh_token, "token");

        config.client_secret_file = Some("/nonexistent/secret".into());
        let partial =
            |name: &str| (name == "CAMELMAILER_POSTMASTER_CLIENT_ID").then(|| "id".to_string());
        assert!(postmaster_credentials(&config, &partial)
            .unwrap_err()
            .contains("cannot read"));
        let snds = camelmailer_config::Snds::default();
        assert!(snds_data_url(&snds, &none).unwrap().is_none());
    }
}
