//! Inbox placement tests with seed mailboxes (`deliverability_feedback.placement`).
//!
//! A round sends one test message from every sending IP (every IP pool
//! address) to every configured seed, through the normal queue: the test
//! carries an `X-Camelmailer-Placement-Test` header with a random token, and
//! the worker binds the IP the test names (see `Worker::placement_address`).
//! After `check_after_seconds` the runner logs in to each seed over IMAP,
//! read-only, looks for the token in the inbox and the spam folder, and
//! records inbox, spam or missing together with the provider's
//! `Authentication-Results` (SPF, DKIM, DMARC) and the connecting IP and
//! EHLO name from its `Received` line.
//!
//! It only measures. It never opens, clicks, replies to, flags, moves or
//! deletes a message: the IMAP client has no command that could
//! ([`crate::imap`]). The tests are plain text, so there is no tracking
//! pixel or rewritten link.

use std::collections::BTreeMap;
use std::time::Duration;

use camelmailer_config::{Placement, Seed, SeedAuth, SeedProvider};
use camelmailer_core::feedback::{placement_status, provider, source};
use camelmailer_core::{
    message::parse_headers, AdminStore, FeedbackStore, MessageScope, NewPlacementTest,
    PlacementResult, PlacementTest, QueuedMessage,
};
use chrono::Utc;

use crate::feedback::secret;
use crate::imap::{ImapClient, ImapError};

/// The header that marks a test and carries its token.
pub const PLACEMENT_HEADER: &str = "X-Camelmailer-Placement-Test";
/// How long one IMAP operation may take.
const IMAP_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a claimed lookup may run before another worker may retry it.
const CHECK_LEASE_SECONDS: i64 = 600;

/// IMAP defaults per seed provider.
pub fn imap_endpoint(seed: &Seed) -> Option<(String, u16, bool)> {
    let host = seed.imap_host.clone().or_else(|| {
        match seed.provider {
            SeedProvider::Gmail => Some("imap.gmail.com"),
            // https://support.microsoft.com/en-us/office/pop-imap-and-smtp-settings-for-outlook-com-d088b986-291d-42b8-9564-9c414e2aa040
            SeedProvider::Outlook => Some("outlook.office365.com"),
            // https://help.yahoo.com/kb/SLN4075.html
            SeedProvider::Yahoo => Some("imap.mail.yahoo.com"),
            SeedProvider::Imap => None,
        }
        .map(str::to_string)
    })?;
    Some((
        host,
        seed.imap_port.unwrap_or(993),
        seed.imap_tls.unwrap_or(true),
    ))
}

/// The provider group a seed measures.
pub fn seed_group(seed: &Seed) -> &'static str {
    match seed.provider {
        SeedProvider::Gmail => provider::GOOGLE,
        SeedProvider::Outlook => provider::MICROSOFT,
        SeedProvider::Yahoo => provider::YAHOO,
        SeedProvider::Imap => provider::OTHER,
    }
}

fn seed_provider_name(seed: &Seed) -> &'static str {
    match seed.provider {
        SeedProvider::Gmail => "gmail",
        SeedProvider::Outlook => "outlook",
        SeedProvider::Yahoo => "yahoo",
        SeedProvider::Imap => "imap",
    }
}

/// The spam folder names tried when no mailbox carries `\Junk`.
fn junk_fallbacks(seed: &Seed) -> Vec<String> {
    if let Some(folder) = &seed.junk_folder {
        return vec![folder.clone()];
    }
    match seed.provider {
        SeedProvider::Gmail => vec!["[Gmail]/Spam".into(), "[Google Mail]/Spam".into()],
        SeedProvider::Outlook => vec!["Junk".into(), "Junk Email".into()],
        SeedProvider::Yahoo => vec!["Bulk".into(), "Bulk Mail".into()],
        SeedProvider::Imap => vec!["Junk".into(), "Spam".into()],
    }
}

/// What the receiving provider recorded about a test.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeaderFindings {
    pub spf: Option<String>,
    pub dkim: Option<String>,
    pub dmarc: Option<String>,
    pub received_ip: Option<String>,
    pub helo: Option<String>,
}

/// `spf=pass` → `pass`, from the first (topmost) `Authentication-Results`.
fn auth_result(value: &str, method: &str) -> Option<String> {
    let needle = format!("{method}=");
    value
        .split(|c: char| c == ';' || c.is_whitespace())
        .find_map(|part| part.trim().strip_prefix(&needle))
        .map(|v| v.trim_end_matches(';').to_ascii_lowercase())
        .filter(|v| !v.is_empty())
}

/// The IP literals in a `Received` value (`[192.0.2.1]`, `(192.0.2.1)`).
fn ip_literals(value: &str) -> Vec<String> {
    value
        .split(['[', ']', '(', ')', ' ', ',', ';'])
        .map(|t| t.trim().trim_start_matches("IPv6:"))
        .filter(|t| t.parse::<std::net::IpAddr>().is_ok())
        .map(str::to_string)
        .collect()
}

/// Read SPF, DKIM and DMARC from the topmost `Authentication-Results`, and
/// the connecting IP and EHLO name from the `Received` line that names
/// `expected_ip` (else the topmost one with an IP literal).
pub fn analyse_header(header: &[u8], expected_ip: &str) -> HeaderFindings {
    let headers = parse_headers(header);
    let mut findings = HeaderFindings::default();
    if let Some((_, value)) = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authentication-results"))
    {
        findings.spf = auth_result(value, "spf");
        findings.dkim = auth_result(value, "dkim");
        findings.dmarc = auth_result(value, "dmarc");
    }
    let received: Vec<&String> = headers
        .iter()
        .filter(|(name, _)| name.eq_ignore_ascii_case("received"))
        .map(|(_, value)| value)
        .collect();
    let line = received
        .iter()
        .find(|value| ip_literals(value).iter().any(|ip| ip == expected_ip))
        .or_else(|| received.iter().find(|value| !ip_literals(value).is_empty()));
    if let Some(line) = line {
        let ips = ip_literals(line);
        findings.received_ip = ips
            .iter()
            .find(|ip| *ip == expected_ip)
            .or(ips.first())
            .cloned();
        findings.helo = line
            .trim_start()
            .strip_prefix("from ")
            .and_then(|rest| rest.split_whitespace().next())
            .map(|helo| helo.trim_end_matches('.').to_string());
    }
    findings
}

/// The raw test message.
pub fn test_message(
    from: &str,
    seed: &str,
    ip: &str,
    token: &str,
    now: chrono::DateTime<Utc>,
) -> Vec<u8> {
    let domain = from.rsplit('@').next().unwrap_or("localhost");
    format!(
        "From: {from}\r\n\
         To: {seed}\r\n\
         Subject: [Placement test] {ip} {short}\r\n\
         Date: {date}\r\n\
         Message-ID: <placement-{token}@{domain}>\r\n\
         MIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\
         Content-Transfer-Encoding: 7bit\r\n\
         Auto-Submitted: auto-generated\r\n\
         {PLACEMENT_HEADER}: {token}\r\n\
         \r\n\
         This is an automated inbox placement test sent by Camelmailer from the\r\n\
         sending address {ip}. It measures whether mail from this address reaches\r\n\
         the inbox. Nothing needs to be done with it.\r\n",
        short = &token[..token.len().min(8)],
        date = now.to_rfc2822(),
    )
    .into_bytes()
}

/// Seed credentials, resolved at use.
async fn access(
    http: &reqwest::Client,
    seed: &Seed,
    lookup: &(dyn Fn(&str) -> Option<String> + Send + Sync),
) -> Result<(String, SeedAuth), String> {
    let read =
        |env: &Option<String>, file: &Option<String>, what: &str| -> Result<String, String> {
            let from_env = env
                .as_deref()
                .and_then(lookup)
                .map(|v| v.trim().to_string());
            if let Some(value) = from_env.filter(|v| !v.is_empty()) {
                return Ok(value);
            }
            secret("", file.as_deref(), &|_| None)?
                .ok_or_else(|| format!("no {what} configured for {}", seed.address))
        };
    match seed.auth {
        SeedAuth::AppPassword => {
            if seed.provider == SeedProvider::Outlook {
                return Err(
                    "Outlook.com accepts only OAuth2 (XOAUTH2) for IMAP since basic \
                     authentication ended on 2024-09-16; set auth: xoauth2"
                        .into(),
                );
            }
            Ok((
                read(&seed.password_env, &seed.password_file, "app password")?,
                SeedAuth::AppPassword,
            ))
        }
        SeedAuth::Xoauth2 => {
            let client_id = seed
                .client_id
                .clone()
                .ok_or_else(|| format!("no client_id configured for {}", seed.address))?;
            let client_secret = read(
                &seed.client_secret_env,
                &seed.client_secret_file,
                "client secret",
            )?;
            let refresh_token = read(
                &seed.refresh_token_env,
                &seed.refresh_token_file,
                "refresh token",
            )?;
            let (token_url, scope) = match seed.provider {
                SeedProvider::Gmail => (
                    "https://oauth2.googleapis.com/token",
                    "https://mail.google.com/",
                ),
                SeedProvider::Outlook => (
                    "https://login.microsoftonline.com/consumers/oauth2/v2.0/token",
                    "https://outlook.office.com/IMAP.AccessAsUser.All offline_access",
                ),
                _ => ("", ""),
            };
            let token_url = seed
                .token_url
                .clone()
                .unwrap_or_else(|| token_url.to_string());
            if token_url.is_empty() {
                return Err(format!("no token_url configured for {}", seed.address));
            }
            let scope = seed.scope.clone().unwrap_or_else(|| scope.to_string());
            let mut form = vec![
                ("client_id", client_id),
                ("client_secret", client_secret),
                ("refresh_token", refresh_token),
                ("grant_type", "refresh_token".to_string()),
            ];
            if !scope.is_empty() {
                form.push(("scope", scope));
            }
            let response = http
                .post(&token_url)
                .form(&form)
                .send()
                .await
                .map_err(|e| format!("token request failed: {}", e.without_url()))?;
            let status = response.status();
            let body: serde_json::Value = response
                .json()
                .await
                .map_err(|e| format!("token response unreadable: {e}"))?;
            if !status.is_success() {
                let code = body
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(format!("token request refused (HTTP {status}, {code})"));
            }
            body.get("access_token")
                .and_then(|v| v.as_str())
                .map(|token| (token.to_string(), SeedAuth::Xoauth2))
                .ok_or_else(|| "token response without access_token".into())
        }
    }
}

/// Look up every test of one seed in a single IMAP session.
async fn check_seed(
    http: &reqwest::Client,
    seed: &Seed,
    tests: &[PlacementTest],
    lookup: &(dyn Fn(&str) -> Option<String> + Send + Sync),
) -> Result<BTreeMap<i64, (String, Option<String>, HeaderFindings)>, String> {
    let (host, port, tls) = imap_endpoint(seed)
        .ok_or_else(|| format!("no imap_host configured for {}", seed.address))?;
    let (secret, auth) = access(http, seed, lookup).await?;
    let user = seed
        .username
        .clone()
        .unwrap_or_else(|| seed.address.clone());
    let imap_error = |e: ImapError| e.to_string();
    let mut client = ImapClient::connect(&host, port, tls, IMAP_TIMEOUT)
        .await
        .map_err(imap_error)?;
    match auth {
        SeedAuth::AppPassword => client.login(&user, &secret).await,
        SeedAuth::Xoauth2 => client.authenticate_xoauth2(&user, &secret).await,
    }
    .map_err(imap_error)?;

    let mailboxes = client.list().await.map_err(imap_error)?;
    let junk = mailboxes
        .iter()
        .find(|(attributes, _)| attributes.iter().any(|a| a.eq_ignore_ascii_case("\\Junk")))
        .map(|(_, name)| name.clone())
        .or_else(|| {
            junk_fallbacks(seed)
                .into_iter()
                .find(|candidate| mailboxes.iter().any(|(_, name)| name == candidate))
        });

    let mut found = BTreeMap::new();
    let mut folders = vec![("INBOX".to_string(), placement_status::INBOX)];
    if let Some(junk) = junk {
        folders.push((junk, placement_status::SPAM));
    }
    for (folder, status) in &folders {
        client.examine(folder).await.map_err(imap_error)?;
        for test in tests {
            if found.contains_key(&test.id) {
                continue;
            }
            let uids = client
                .search_header(PLACEMENT_HEADER, &test.token)
                .await
                .map_err(imap_error)?;
            if let Some(uid) = uids.first() {
                let header = client.fetch_header(*uid).await.map_err(imap_error)?;
                found.insert(
                    test.id,
                    (
                        status.to_string(),
                        Some(folder.clone()),
                        analyse_header(&header, &test.ip),
                    ),
                );
            }
        }
    }
    client.logout().await;
    Ok(found)
}

type SecretLookup = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The placement scheduler of one worker process.
pub struct PlacementRunner {
    config: Placement,
    store: camelmailer_db::PgStore,
    sink: camelmailer_db::PgMessageSink,
    http: reqwest::Client,
    lookup: SecretLookup,
}

impl PlacementRunner {
    pub fn new(config: &camelmailer_config::Config, store: camelmailer_db::PgStore) -> Self {
        Self {
            config: config.deliverability_feedback.placement.clone(),
            sink: camelmailer_db::PgMessageSink::new(store.clone()),
            store,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("reqwest client"),
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

    /// Whether placement tests are configured at all.
    pub fn enabled(&self) -> bool {
        !self.config.seeds.is_empty()
            && !self.config.server.trim().is_empty()
            && self.config.from_address.contains('@')
    }

    /// One scheduler round: request a scheduled round when one is due, send
    /// every requested round, and look up every test that is due.
    pub async fn run_once(&self) -> Result<(), String> {
        let enabled = self.enabled();
        FeedbackStore::set_feedback_source_enabled(&self.store, source::PLACEMENT, enabled)
            .await
            .map_err(|e| e.to_string())?;
        if !enabled {
            return Ok(());
        }
        if self.config.interval_seconds > 0 {
            let last = FeedbackStore::last_placement_round(&self.store)
                .await
                .map_err(|e| e.to_string())?;
            let due = last.is_none_or(|round| {
                Utc::now() - round.requested_at
                    >= chrono::Duration::seconds(i64::from(self.config.interval_seconds))
            });
            if due {
                FeedbackStore::request_placement_round(
                    &self.store,
                    "schedule",
                    i64::from(self.config.min_round_interval_seconds),
                )
                .await
                .map_err(|e| e.to_string())?;
            }
        }
        while let Some(round) = FeedbackStore::start_placement_round(&self.store)
            .await
            .map_err(|e| e.to_string())?
        {
            if let Err(error) = self.send_round(round.id).await {
                tracing::warn!(round = round.id, %error, "placement round could not be sent");
            }
        }
        self.check_due().await
    }

    async fn send_round(&self, round_id: i64) -> Result<usize, String> {
        let (org, server) = self
            .config
            .server
            .split_once('/')
            .ok_or("deliverability_feedback.placement.server must be organization/server")?;
        let organization = AdminStore::organization_by_permalink(&self.store, org)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("organization {org} not found"))?;
        let server = AdminStore::server_by_permalink(&self.store, organization.id, server)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("server {} not found", self.config.server))?;
        let from_domain = self
            .config
            .from_address
            .rsplit('@')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let domain_id = AdminStore::list_domains(&self.store, server.id)
            .await
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|d| d.name.eq_ignore_ascii_case(&from_domain))
            .map(|d| d.id);
        if domain_id.is_none() {
            tracing::warn!(
                domain = %from_domain,
                "the placement tests' sender domain is not on the placement server; they go out unsigned"
            );
        }

        let mut addresses = Vec::new();
        for pool in AdminStore::list_ip_pools(&self.store)
            .await
            .map_err(|e| e.to_string())?
        {
            addresses.extend(
                AdminStore::list_ip_addresses(&self.store, pool.id)
                    .await
                    .map_err(|e| e.to_string())?,
            );
        }
        let check_at = Utc::now()
            + chrono::Duration::seconds(i64::from(self.config.check_after_seconds.max(60)));
        let mut new = Vec::new();
        for address in &addresses {
            for seed in &self.config.seeds {
                new.push(NewPlacementTest {
                    round_id,
                    token: camelmailer_core::token::generate_token(24),
                    server_id: server.id,
                    ip_pool_id: address.ip_pool_id,
                    ip_address_id: address.id,
                    ip: address.ipv4.clone(),
                    seed_address: seed.address.clone(),
                    seed_provider: seed_provider_name(seed).into(),
                    provider: seed_group(seed).into(),
                    check_at,
                });
            }
        }
        let tests = FeedbackStore::create_placement_tests(&self.store, &new)
            .await
            .map_err(|e| e.to_string())?;
        for test in &tests {
            let message = QueuedMessage {
                server_id: test.server_id,
                rcpt_to: test.seed_address.clone(),
                mail_from: self.config.from_address.clone(),
                raw_message: test_message(
                    &self.config.from_address,
                    &test.seed_address,
                    &test.ip,
                    &test.token,
                    Utc::now(),
                ),
                received_with_ssl: false,
                scope: MessageScope::Outgoing,
                bounce: false,
                domain_id,
                credential_id: None,
                route_id: None,
                tag: Some("placement-test".into()),
                metadata: None,
                stream_id: None,
            };
            match self.sink.insert_message(&message).await {
                Ok(message_id) => {
                    let _ = FeedbackStore::set_placement_message(&self.store, test.id, message_id)
                        .await;
                }
                Err(error) => {
                    let _ = FeedbackStore::record_placement_result(
                        &self.store,
                        test.id,
                        &PlacementResult {
                            status: placement_status::ERROR.into(),
                            detail: format!("could not queue the test: {error}"),
                            ..Default::default()
                        },
                    )
                    .await;
                }
            }
        }
        tracing::info!(
            round = round_id,
            tests = tests.len(),
            "placement round queued"
        );
        Ok(tests.len())
    }

    async fn check_due(&self) -> Result<(), String> {
        let due = FeedbackStore::claim_due_placement_tests(&self.store, 500, CHECK_LEASE_SECONDS)
            .await
            .map_err(|e| e.to_string())?;
        let mut by_seed: BTreeMap<String, Vec<PlacementTest>> = BTreeMap::new();
        for test in due {
            by_seed
                .entry(test.seed_address.clone())
                .or_default()
                .push(test);
        }
        for (address, tests) in by_seed {
            let seed = self.config.seeds.iter().find(|s| s.address == address);
            let outcome = match seed {
                Some(seed) => check_seed(&self.http, seed, &tests, &*self.lookup).await,
                None => Err(format!("seed {address} is no longer configured")),
            };
            for test in &tests {
                let last_check = test.checks + 1 >= self.config.max_checks.max(1) as i32;
                let result = match &outcome {
                    Ok(found) => match found.get(&test.id) {
                        Some((status, folder, findings)) => PlacementResult {
                            status: status.clone(),
                            folder: folder.clone(),
                            spf: findings.spf.clone(),
                            dkim: findings.dkim.clone(),
                            dmarc: findings.dmarc.clone(),
                            received_ip: findings.received_ip.clone(),
                            helo: findings.helo.clone(),
                            detail: String::new(),
                            check_again_at: None,
                        },
                        None if last_check => PlacementResult {
                            status: placement_status::MISSING.into(),
                            detail: "not in the inbox or the spam folder".into(),
                            ..Default::default()
                        },
                        None => self.again("not found yet"),
                    },
                    Err(error) if last_check => PlacementResult {
                        status: placement_status::ERROR.into(),
                        detail: error.clone(),
                        ..Default::default()
                    },
                    Err(error) => self.again(error),
                };
                if let Err(error) =
                    FeedbackStore::record_placement_result(&self.store, test.id, &result).await
                {
                    tracing::warn!(%error, test = test.id, "could not record a placement result");
                }
            }
        }
        Ok(())
    }

    fn again(&self, detail: &str) -> PlacementResult {
        PlacementResult {
            status: placement_status::PENDING.into(),
            detail: detail.to_string(),
            check_again_at: Some(
                Utc::now()
                    + chrono::Duration::seconds(i64::from(self.config.check_after_seconds.max(60))),
            ),
            ..Default::default()
        }
    }
}

/// Start the placement scheduler when seeds are configured.
pub fn spawn(
    config: &camelmailer_config::Config,
    store: camelmailer_db::PgStore,
) -> Option<tokio::task::JoinHandle<()>> {
    let runner = PlacementRunner::new(config, store);
    if !runner.enabled() {
        let store = runner.store.clone();
        tokio::spawn(async move {
            let _ =
                FeedbackStore::set_feedback_source_enabled(&store, source::PLACEMENT, false).await;
        });
        return None;
    }
    Some(tokio::spawn(async move {
        loop {
            if let Err(error) = runner.run_once().await {
                tracing::warn!(%error, "placement tests");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_receiving_providers_verdicts_and_received_line_are_read() {
        let header = b"Delivered-To: seed@gmail.com\r\n\
Received: by 2002:a05:6a10:1234 with SMTP id abc;\r\n        Fri, 13 Nov 2026 10:00:01 -0800 (PST)\r\n\
Received: from out1.mail.example.com (out1.mail.example.com. [192.0.2.10])\r\n        by mx.google.com with ESMTPS id x;\r\n        Fri, 13 Nov 2026 10:00:00 -0800 (PST)\r\n\
Authentication-Results: mx.google.com;\r\n       dkim=pass header.i=@example.com header.s=cm;\r\n       spf=pass (google.com: domain designates 192.0.2.10 as permitted sender) smtp.mailfrom=x@example.com;\r\n       dmarc=pass (p=REJECT) header.from=example.com\r\n\
Subject: [Placement test] 192.0.2.10 abc\r\n\r\n";
        let findings = analyse_header(header, "192.0.2.10");
        assert_eq!(findings.spf.as_deref(), Some("pass"));
        assert_eq!(findings.dkim.as_deref(), Some("pass"));
        assert_eq!(findings.dmarc.as_deref(), Some("pass"));
        assert_eq!(findings.received_ip.as_deref(), Some("192.0.2.10"));
        assert_eq!(findings.helo.as_deref(), Some("out1.mail.example.com"));

        // Outlook writes the IP in parentheses.
        let outlook = b"Received: from out1.mail.example.com (192.0.2.10) by\r\n AM0PR.outlook.office365.com (10.0.0.1) with Microsoft SMTP Server\r\n\
Authentication-Results: spf=softfail (sender IP is 192.0.2.10) smtp.mailfrom=example.com; dkim=none; dmarc=fail action=oreject\r\n\r\n";
        let findings = analyse_header(outlook, "192.0.2.10");
        assert_eq!(findings.spf.as_deref(), Some("softfail"));
        assert_eq!(findings.dkim.as_deref(), Some("none"));
        assert_eq!(findings.dmarc.as_deref(), Some("fail"));
        assert_eq!(findings.received_ip.as_deref(), Some("192.0.2.10"));
    }

    #[test]
    fn test_messages_are_plain_text_and_identified() {
        let raw = test_message(
            "placement@example.com",
            "seed@gmail.com",
            "192.0.2.10",
            "abcdefghijklmnopqrstuvwx",
            Utc::now(),
        );
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("X-Camelmailer-Placement-Test: abcdefghijklmnopqrstuvwx\r\n"));
        assert!(text.contains("Subject: [Placement test] 192.0.2.10 abcdefgh\r\n"));
        assert!(text.contains("Content-Type: text/plain"));
        assert!(text.contains("Auto-Submitted: auto-generated"));
        assert!(!text.contains("<html"));
    }

    #[test]
    fn provider_defaults_follow_the_providers_docs() {
        let seed = |provider| Seed {
            provider,
            address: "s@x.example".into(),
            ..Default::default()
        };
        assert_eq!(
            imap_endpoint(&seed(SeedProvider::Gmail)),
            Some(("imap.gmail.com".into(), 993, true))
        );
        assert_eq!(
            imap_endpoint(&seed(SeedProvider::Outlook)).unwrap().0,
            "outlook.office365.com"
        );
        assert_eq!(
            imap_endpoint(&seed(SeedProvider::Yahoo)).unwrap().0,
            "imap.mail.yahoo.com"
        );
        assert_eq!(imap_endpoint(&seed(SeedProvider::Imap)), None);
        assert_eq!(seed_group(&seed(SeedProvider::Outlook)), "microsoft");
    }

    #[tokio::test]
    async fn outlook_with_an_app_password_is_refused_before_connecting() {
        let seed = Seed {
            provider: SeedProvider::Outlook,
            address: "seed@outlook.com".into(),
            auth: SeedAuth::AppPassword,
            ..Default::default()
        };
        let error = access(&reqwest::Client::new(), &seed, &|_| Some("pw".into()))
            .await
            .unwrap_err();
        assert!(error.contains("XOAUTH2"), "{error}");
    }
}
