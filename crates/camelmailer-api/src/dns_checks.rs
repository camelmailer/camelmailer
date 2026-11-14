//! DNS checks for the infrastructure dashboard
//! (`GET /api/v2/admin/dns_checks` and `GET /api/v2/admin/dns_checks/candidate`).
//!
//! Every check names the exact record it expects, what DNS answers today,
//! and an instruction the operator can copy (`set A out1.mail.example.com →
//! 203.0.113.10, TTL 300`). The checks only read DNS; nothing here changes a
//! record, a pool or the configuration.
//!
//! What is checked:
//!
//! - the inbound host name (`cluster.inbound_hostname`) has A/AAAA records
//!   for the load balancer (`cluster.load_balancer_address`), or for the
//!   public addresses of the live SMTP nodes when there is no balancer;
//! - every sending address (both values of every IP pool address, and the
//!   public addresses of the live workers for servers without a pool) has a
//!   PTR record, the PTR name resolves back to the address (forward-confirmed
//!   reverse DNS), and the name the worker greets with (EHLO) is that PTR
//!   name;
//! - the SPF policy of the return-path domain and of every verified tenant
//!   domain lets every sending address pass, with the exact `ip4:`/`ip6:`
//!   mechanisms to add when it does not;
//! - the return-path domain's MX leads back to the installation, tenant
//!   domains with an inbound route have their MX on the installation (a
//!   warning only), and the tracking host name resolves.
//!
//! Lookups are bounded: each one has a timeout, at most [`CONCURRENCY`]
//! subjects are checked at once, and the full result is kept for
//! [`CACHE_SECONDS`] unless the caller asks for a refresh.

use crate::app::{
    render_store_error, render_success, render_validation_error, ApiResponse, ApiState,
    RequestStart,
};
use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use camelmailer_core::spf::{SpfResult, SpfVerdict};
use camelmailer_core::{DnsError, SpfResolver};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How many subjects (addresses, domains) are checked at once.
pub const CONCURRENCY: usize = 8;
/// How long the full result is reused without `?refresh=1`.
pub const CACHE_SECONDS: u64 = 60;
/// At most this many tenant domains are checked; the result says when the
/// list was cut.
pub const MAX_TENANT_DOMAINS: usize = 100;
/// The TTL suggested in instructions.
const TTL: u32 = 300;

/// The lookups the checks need.
#[async_trait]
pub trait DnsCheckResolver: Send + Sync {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError>;
    /// A and AAAA addresses.
    async fn ip_addresses(&self, name: &str) -> Result<Vec<IpAddr>, DnsError>;
    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError>;
    /// PTR names, without the trailing dot.
    async fn ptr(&self, address: IpAddr) -> Result<Vec<String>, DnsError>;
}

/// Lets the SPF evaluator use a [`DnsCheckResolver`].
struct SpfAdapter<'a>(&'a dyn DnsCheckResolver);

#[async_trait]
impl SpfResolver for SpfAdapter<'_> {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        self.0.txt(name).await
    }
    async fn ip_addresses(&self, name: &str) -> Result<Vec<IpAddr>, DnsError> {
        self.0.ip_addresses(name).await
    }
    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError> {
        self.0.mx_hosts(name).await
    }
}

/// `ok`, `warn` or `fail`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

/// One check: the record it expects, what DNS answers, and what to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DnsCheck {
    /// What the check is about: `address`, `hostname` or `domain`.
    pub group: &'static str,
    /// The address, host name or domain.
    pub subject: String,
    /// `a`, `ptr`, `fcrdns`, `ehlo`, `spf`, `mx` or `resolves`.
    pub kind: &'static str,
    pub status: CheckStatus,
    /// What DNS answers today.
    pub found: Vec<String>,
    /// What it should answer.
    pub expected: Vec<String>,
    /// The records to set, one instruction per line, ready to copy. Empty
    /// when the check passes.
    pub instructions: Vec<String>,
    pub detail: String,
}

/// A sending address and the name it should carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendingAddress {
    pub address: IpAddr,
    /// The name the worker greets with from this address, which the PTR
    /// record must carry.
    pub ehlo: String,
    /// `pool` or `host`.
    pub origin: &'static str,
    /// The pool's name, for a pool address.
    pub pool: Option<String>,
}

/// A domain whose SPF policy must authorise the sending addresses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpfDomain {
    pub name: String,
    /// `return_path` or `tenant`.
    pub role: &'static str,
}

/// Everything the checks compare DNS against, read from the configuration
/// and the store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DnsPlan {
    pub inbound_hostname: Option<String>,
    /// Where the inbound host name should point.
    pub inbound_targets: Vec<IpAddr>,
    pub sending: Vec<SendingAddress>,
    pub spf_domains: Vec<SpfDomain>,
    /// The host names the MX of the return-path domain and of inbound
    /// tenant domains may point at.
    pub mx_targets: Vec<String>,
    pub return_path_domain: Option<String>,
    /// Tenant domains with an inbound route.
    pub inbound_domains: Vec<String>,
    pub track_hostname: Option<String>,
    /// Tenant domains left out because of [`MAX_TENANT_DOMAINS`].
    pub truncated_domains: usize,
}

fn family_record(address: &IpAddr) -> &'static str {
    if address.is_ipv4() {
        "A"
    } else {
        "AAAA"
    }
}

fn mechanism(address: &IpAddr) -> String {
    match address {
        IpAddr::V4(v4) => format!("ip4:{v4}"),
        IpAddr::V6(v6) => format!("ip6:{v6}"),
    }
}

/// `set A name → address, TTL 300`.
pub fn forward_instruction(name: &str, address: &IpAddr) -> String {
    format!(
        "set {} {name} → {address}, TTL {TTL}",
        family_record(address)
    )
}

/// `set PTR address → name (at the provider of the address)`.
pub fn ptr_instruction(address: &IpAddr, name: &str) -> String {
    format!("set PTR {address} → {name} (at the provider of the address)")
}

fn lower(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .map(|n| n.trim_end_matches('.').to_ascii_lowercase())
        .collect()
}

fn lookup_detail(error: &DnsError) -> String {
    format!("lookup failed: {error}")
}

/// The A/AAAA check of `name` against `targets`.
async fn forward_check(
    resolver: &dyn DnsCheckResolver,
    group: &'static str,
    name: &str,
    targets: &[IpAddr],
    what: &str,
) -> DnsCheck {
    let expected: Vec<String> = targets.iter().map(ToString::to_string).collect();
    let (status, found, detail) = match resolver.ip_addresses(name).await {
        Err(error) => (CheckStatus::Warn, Vec::new(), lookup_detail(&error)),
        Ok(found) => {
            let missing: Vec<&IpAddr> = targets.iter().filter(|t| !found.contains(t)).collect();
            let extra: Vec<&IpAddr> = found.iter().filter(|f| !targets.contains(f)).collect();
            let shown = found.iter().map(ToString::to_string).collect();
            if targets.is_empty() {
                (
                    CheckStatus::Warn,
                    shown,
                    format!("no {what} is configured to compare against"),
                )
            } else if !missing.is_empty() {
                (
                    CheckStatus::Fail,
                    shown,
                    format!("{name} does not point at every {what}"),
                )
            } else if !extra.is_empty() {
                (
                    CheckStatus::Warn,
                    shown,
                    format!("{name} also points at addresses that are not a {what}; remove them unless they are intended"),
                )
            } else {
                (
                    CheckStatus::Ok,
                    shown,
                    format!("{name} points at the {what}"),
                )
            }
        }
    };
    let instructions = if status == CheckStatus::Fail {
        targets
            .iter()
            .map(|target| forward_instruction(name, target))
            .collect()
    } else {
        Vec::new()
    };
    DnsCheck {
        group,
        subject: name.to_string(),
        kind: "a",
        status,
        found,
        expected,
        instructions,
        detail,
    }
}

/// PTR, forward-confirmed reverse DNS and EHLO of one sending address.
pub async fn address_checks(
    resolver: &dyn DnsCheckResolver,
    sending: &SendingAddress,
) -> Vec<DnsCheck> {
    let address = sending.address;
    let ehlo = sending.ehlo.to_ascii_lowercase();
    let subject = address.to_string();
    let mut checks = Vec::new();
    let ptr = resolver.ptr(address).await.map(lower);

    // PTR
    let (ptr_names, ptr_check) = match &ptr {
        Err(error) => (
            Vec::new(),
            DnsCheck {
                group: "address",
                subject: subject.clone(),
                kind: "ptr",
                status: CheckStatus::Warn,
                found: Vec::new(),
                expected: vec![ehlo.clone()],
                instructions: Vec::new(),
                detail: lookup_detail(error),
            },
        ),
        Ok(names) if names.is_empty() => (
            Vec::new(),
            DnsCheck {
                group: "address",
                subject: subject.clone(),
                kind: "ptr",
                status: CheckStatus::Fail,
                found: Vec::new(),
                expected: vec![ehlo.clone()],
                instructions: vec![ptr_instruction(&address, &ehlo)],
                detail: "no PTR record; receivers such as Microsoft 365 and Gmail refuse or junk mail from an address without one".into(),
            },
        ),
        Ok(names) => (
            names.clone(),
            DnsCheck {
                group: "address",
                subject: subject.clone(),
                kind: "ptr",
                status: CheckStatus::Ok,
                found: names.clone(),
                expected: vec![ehlo.clone()],
                instructions: Vec::new(),
                detail: "has a PTR record".into(),
            },
        ),
    };
    checks.push(ptr_check);

    // Forward-confirmed reverse DNS: a PTR name that resolves back.
    let mut confirmed = false;
    let mut forward_found = BTreeSet::new();
    let mut forward_error = None;
    for name in &ptr_names {
        match resolver.ip_addresses(name).await {
            Ok(addresses) => {
                if addresses.contains(&address) {
                    confirmed = true;
                }
                forward_found.extend(addresses.iter().map(|a| format!("{name} → {a}")));
            }
            Err(error) => forward_error = Some(lookup_detail(&error)),
        }
    }
    let fcrdns = if ptr.is_err() {
        None
    } else if confirmed {
        Some((
            CheckStatus::Ok,
            Vec::new(),
            "the PTR name resolves back to the address".to_string(),
        ))
    } else if let Some(detail) = forward_error.filter(|_| !ptr_names.is_empty()) {
        Some((CheckStatus::Warn, Vec::new(), detail))
    } else {
        // The forward record the PTR name (or, without a PTR, the EHLO name)
        // needs.
        let name = ptr_names.first().cloned().unwrap_or_else(|| ehlo.clone());
        Some((
            CheckStatus::Fail,
            vec![forward_instruction(&name, &address)],
            format!("{name} does not resolve back to {address}"),
        ))
    };
    if let Some((status, instructions, detail)) = fcrdns {
        checks.push(DnsCheck {
            group: "address",
            subject: subject.clone(),
            kind: "fcrdns",
            status,
            found: forward_found.into_iter().collect(),
            expected: vec![format!(
                "{} → {address}",
                ptr_names.first().unwrap_or(&ehlo)
            )],
            instructions,
            detail,
        });
    }

    // EHLO == PTR name
    if !ptr_names.is_empty() {
        let matches = ptr_names.contains(&ehlo);
        checks.push(DnsCheck {
            group: "address",
            subject,
            kind: "ehlo",
            status: if matches { CheckStatus::Ok } else { CheckStatus::Fail },
            found: ptr_names.clone(),
            expected: vec![ehlo.clone()],
            instructions: if matches {
                Vec::new()
            } else {
                vec![ptr_instruction(&address, &ehlo)]
            },
            detail: if matches {
                format!("the worker greets with {ehlo}, the PTR name")
            } else {
                format!(
                    "the worker greets with {ehlo} from this address, but the PTR name is {}; change the PTR record, or the {} so both agree",
                    ptr_names.join(", "),
                    if sending.origin == "pool" {
                        "pool address's hostname (with smtp_client.helo_from_ip_address: true)"
                    } else {
                        "dns.helo_hostname"
                    }
                )
            },
        });
    }
    checks
}

/// Insert `mechanisms` into an SPF record before its final `all` or
/// `redirect=` term, or build a record when there is none.
pub fn spf_record_with(existing: Option<&str>, mechanisms: &[String]) -> String {
    let Some(existing) = existing else {
        return format!("v=spf1 {} ~all", mechanisms.join(" "));
    };
    let mut terms: Vec<String> = existing.split_whitespace().map(String::from).collect();
    let at = terms
        .iter()
        .position(|t| {
            let bare = t
                .trim_start_matches(['+', '-', '~', '?'])
                .to_ascii_lowercase();
            bare == "all" || bare.starts_with("redirect=")
        })
        .unwrap_or(terms.len());
    for (offset, mechanism) in mechanisms.iter().enumerate() {
        terms.insert(at + offset, mechanism.clone());
    }
    terms.join(" ")
}

/// Does every sending address pass the SPF policy of `domain`?
pub async fn spf_check(
    resolver: &dyn DnsCheckResolver,
    domain: &SpfDomain,
    sending: &[IpAddr],
) -> DnsCheck {
    let adapter = SpfAdapter(resolver);
    let memo = crate::dns::MemoizingSpfResolver::new(&adapter);
    let mut missing = Vec::new();
    let mut unverifiable = Vec::new();
    let mut errors = Vec::new();
    for address in sending {
        match camelmailer_core::spf::evaluate_verifiable(&memo, &domain.name, *address).await {
            SpfVerdict::Determined(SpfResult::Pass) => {}
            SpfVerdict::Determined(SpfResult::TempError) => errors.push(*address),
            SpfVerdict::Determined(_) => missing.push(*address),
            SpfVerdict::Unverifiable(_) => unverifiable.push(*address),
        }
    }
    let records: Vec<String> = match resolver.txt(&domain.name).await {
        Ok(txt) => txt
            .into_iter()
            .filter(|t| t.to_ascii_lowercase().starts_with("v=spf1"))
            .collect(),
        Err(_) => Vec::new(),
    };
    let expected: Vec<String> = sending.iter().map(mechanism).collect();
    let (status, instructions, detail) = if !missing.is_empty() {
        let add: Vec<String> = missing.iter().map(mechanism).collect();
        let record = spf_record_with(records.first().map(String::as_str), &add);
        (
            CheckStatus::Fail,
            vec![format!("set TXT {} → \"{record}\", TTL {TTL}", domain.name)],
            format!(
                "the SPF policy of {} does not let {} pass",
                domain.name,
                missing
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        )
    } else if !errors.is_empty() {
        (
            CheckStatus::Warn,
            Vec::new(),
            "the SPF policy could not be evaluated (DNS lookup failed)".into(),
        )
    } else if !unverifiable.is_empty() {
        (
            CheckStatus::Warn,
            Vec::new(),
            "the SPF policy uses terms that cannot be evaluated here (macros or ptr); verify it by other means".into(),
        )
    } else {
        (
            CheckStatus::Ok,
            Vec::new(),
            "every sending address passes SPF".into(),
        )
    };
    DnsCheck {
        group: "domain",
        subject: domain.name.clone(),
        kind: "spf",
        status,
        found: records,
        expected,
        instructions,
        detail: format!("{} ({})", detail, domain.role.replace('_', "-")),
    }
}

/// Does the MX of `domain` point at one of `targets`?
async fn mx_check(
    resolver: &dyn DnsCheckResolver,
    domain: &str,
    targets: &[String],
    failing: CheckStatus,
    what: &str,
) -> DnsCheck {
    let (status, found, detail) = match resolver.mx_hosts(domain).await {
        Err(error) => (CheckStatus::Warn, Vec::new(), lookup_detail(&error)),
        Ok(hosts) => {
            let hosts = lower(hosts);
            if targets.is_empty() {
                (
                    CheckStatus::Warn,
                    hosts,
                    "no inbound host name is configured to compare against (cluster.inbound_hostname or dns.mx_records)".into(),
                )
            } else if hosts.iter().any(|h| targets.contains(h)) {
                (
                    CheckStatus::Ok,
                    hosts,
                    format!("{what} reaches this installation"),
                )
            } else {
                (failing, hosts, format!("the MX of {domain} does not point at this installation, so {what} does not arrive here"))
            }
        }
    };
    DnsCheck {
        group: "domain",
        subject: domain.to_string(),
        kind: "mx",
        status,
        found,
        expected: targets.to_vec(),
        instructions: if matches!(status, CheckStatus::Fail | CheckStatus::Warn)
            && !targets.is_empty()
            && !detail.starts_with("lookup failed")
        {
            vec![format!("set MX {domain} → 10 {}, TTL {TTL}", targets[0])]
        } else {
            Vec::new()
        },
        detail,
    }
}

/// Does `name` resolve at all?
async fn resolves_check(resolver: &dyn DnsCheckResolver, name: &str, what: &str) -> DnsCheck {
    let (status, found, detail) = match resolver.ip_addresses(name).await {
        Err(error) => (CheckStatus::Warn, Vec::new(), lookup_detail(&error)),
        Ok(found) if found.is_empty() => (
            CheckStatus::Fail,
            Vec::new(),
            format!("{name} does not resolve; {what} will not work"),
        ),
        Ok(found) => (
            CheckStatus::Ok,
            found.iter().map(ToString::to_string).collect(),
            format!("{name} resolves"),
        ),
    };
    DnsCheck {
        group: "hostname",
        subject: name.to_string(),
        kind: "resolves",
        status,
        found,
        expected: Vec::new(),
        instructions: if status == CheckStatus::Fail {
            vec![format!(
                "set CNAME {name} → the host name of the web server (or A/AAAA records), TTL {TTL}"
            )]
        } else {
            Vec::new()
        },
        detail,
    }
}

/// Run every check of `plan`, at most [`CONCURRENCY`] subjects at a time.
pub async fn run_checks(resolver: &dyn DnsCheckResolver, plan: &DnsPlan) -> Vec<DnsCheck> {
    use futures_util::stream::{self, StreamExt};
    type Job<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Vec<DnsCheck>> + Send + 'a>>;
    let mut jobs: Vec<Job<'_>> = Vec::new();
    if let Some(name) = &plan.inbound_hostname {
        jobs.push(Box::pin(async move {
            vec![
                forward_check(
                    resolver,
                    "hostname",
                    name,
                    &plan.inbound_targets,
                    "load balancer or SMTP node address",
                )
                .await,
            ]
        }));
    }
    for sending in &plan.sending {
        jobs.push(Box::pin(address_checks(resolver, sending)));
    }
    let addresses: Vec<IpAddr> = plan.sending.iter().map(|s| s.address).collect();
    for domain in &plan.spf_domains {
        let addresses = addresses.clone();
        jobs.push(Box::pin(async move {
            if addresses.is_empty() {
                Vec::new()
            } else {
                vec![spf_check(resolver, domain, &addresses).await]
            }
        }));
    }
    if let Some(domain) = &plan.return_path_domain {
        jobs.push(Box::pin(async move {
            vec![
                mx_check(
                    resolver,
                    domain,
                    &plan.mx_targets,
                    CheckStatus::Fail,
                    "bounce mail to the return path",
                )
                .await,
            ]
        }));
    }
    for domain in &plan.inbound_domains {
        jobs.push(Box::pin(async move {
            vec![
                mx_check(
                    resolver,
                    domain,
                    &plan.mx_targets,
                    CheckStatus::Warn,
                    "inbound mail",
                )
                .await,
            ]
        }));
    }
    if let Some(name) = &plan.track_hostname {
        jobs.push(Box::pin(async move {
            vec![resolves_check(resolver, name, "click and open tracking").await]
        }));
    }
    stream::iter(jobs)
        .buffered(CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect()
}

/// The JSON document of a check run: the checks, grouped by subject, and
/// counts per status.
pub fn checks_json(checks: &[DnsCheck], plan: &DnsPlan) -> Value {
    let count = |status: CheckStatus| checks.iter().filter(|c| c.status == status).count();
    let mut groups: Vec<Value> = Vec::new();
    for check in checks {
        let key = (check.group, check.subject.as_str());
        match groups
            .iter_mut()
            .find(|g| g["group"] == key.0 && g["subject"] == key.1)
        {
            Some(group) => group["checks"].as_array_mut().unwrap().push(json!(check)),
            None => groups.push(json!({
                "group": check.group,
                "subject": check.subject,
                "checks": [check],
            })),
        }
    }
    for group in &mut groups {
        let statuses: Vec<String> = group["checks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["status"].as_str().unwrap_or("").to_string())
            .collect();
        group["status"] = json!(if statuses.iter().any(|s| s == "fail") {
            "fail"
        } else if statuses.iter().any(|s| s == "warn") {
            "warn"
        } else {
            "ok"
        });
    }
    // Each record once, in the order the checks name them (the forward
    // record of a new host name is asked for by two checks).
    let mut instructions: Vec<String> = Vec::new();
    for instruction in checks
        .iter()
        .filter(|c| c.status != CheckStatus::Ok)
        .flat_map(|c| c.instructions.iter())
    {
        if !instructions.contains(instruction) {
            instructions.push(instruction.clone());
        }
    }
    json!({
        "summary": { "ok": count(CheckStatus::Ok), "warn": count(CheckStatus::Warn), "fail": count(CheckStatus::Fail) },
        "groups": groups,
        "instructions": instructions,
        "truncated_domains": plan.truncated_domains,
    })
}

/// The EHLO name a pool address greets with.
fn pool_ehlo(config: &camelmailer_config::Config, hostname: &str) -> String {
    let fallback = helo_fallback(config);
    if config.smtp_client.helo_from_ip_address
        && camelmailer_core::dns::is_valid_helo_hostname(hostname)
    {
        hostname.to_ascii_lowercase()
    } else {
        fallback
    }
}

fn helo_fallback(config: &camelmailer_config::Config) -> String {
    config
        .dns
        .helo_hostname
        .clone()
        .unwrap_or_else(|| config.camelmailer.smtp_hostname.clone())
        .to_ascii_lowercase()
}

/// Read the plan from the configuration and the store.
pub async fn build_plan(state: &ApiState) -> Result<DnsPlan, camelmailer_core::StoreError> {
    let config = &state.config;
    let store = &state.store;
    let now = chrono::Utc::now();
    let nodes = store.list_cluster_nodes().await?;
    let live = |role: &str| {
        nodes
            .iter()
            .filter(move |n| {
                n.role == role
                    && crate::cluster_api::node_status(n, now, config.cluster.node_stale_seconds)
                        == "live"
            })
            .flat_map(|n| {
                n.details["public_addresses"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .filter_map(|v| v.as_str().and_then(|s| s.parse::<IpAddr>().ok()))
            .collect::<BTreeSet<IpAddr>>()
    };

    let mut plan = DnsPlan {
        inbound_hostname: config.cluster.inbound_host(),
        ..DnsPlan::default()
    };
    plan.inbound_targets = match config.cluster.load_balancer().and_then(|a| a.parse().ok()) {
        Some(lb) => vec![lb],
        None => live("smtp").into_iter().collect(),
    };

    for pool in store.list_ip_pools().await? {
        for address in store.list_ip_addresses(pool.id).await? {
            let ehlo = pool_ehlo(config, &address.hostname);
            for value in std::iter::once(Some(address.ipv4.as_str()))
                .chain(std::iter::once(address.ipv6.as_deref()))
                .flatten()
            {
                if let Ok(ip) = value.trim().parse::<IpAddr>() {
                    if !plan.sending.iter().any(|s| s.address == ip) {
                        plan.sending.push(SendingAddress {
                            address: ip,
                            ehlo: ehlo.clone(),
                            origin: "pool",
                            pool: Some(pool.name.clone()),
                        });
                    }
                }
            }
        }
    }
    for ip in live("worker") {
        if !plan.sending.iter().any(|s| s.address == ip) {
            plan.sending.push(SendingAddress {
                address: ip,
                ehlo: helo_fallback(config),
                origin: "host",
                pool: None,
            });
        }
    }

    plan.mx_targets = if config.dns.mx_records.is_empty() {
        plan.inbound_hostname.iter().cloned().collect()
    } else {
        config
            .dns
            .mx_records
            .iter()
            .map(|m| m.trim_end_matches('.').to_ascii_lowercase())
            .collect()
    };
    if config.dns.return_path_envelope {
        if let Some(domain) = config.dns.normalized_return_path_domain() {
            plan.spf_domains.push(SpfDomain {
                name: domain.clone(),
                role: "return_path",
            });
            plan.return_path_domain = Some(domain);
        }
    }

    let mut seen = BTreeSet::new();
    let mut inbound = BTreeSet::new();
    'organizations: for organization in store.list_organizations().await? {
        for server in store.servers_for_organization(organization.id).await? {
            let routes = store.list_routes(server.id).await?;
            for domain in store.list_domains(server.id).await? {
                if !domain.verified {
                    continue;
                }
                let name = domain.name.to_ascii_lowercase();
                if routes.iter().any(|r| r.domain_id == Some(domain.id)) {
                    inbound.insert(name.clone());
                }
                if seen.contains(&name) {
                    continue;
                }
                if seen.len() >= MAX_TENANT_DOMAINS {
                    plan.truncated_domains += 1;
                    if plan.truncated_domains > 10_000 {
                        break 'organizations;
                    }
                    continue;
                }
                seen.insert(name.clone());
                plan.spf_domains.push(SpfDomain {
                    name,
                    role: "tenant",
                });
            }
        }
    }
    plan.inbound_domains = inbound.into_iter().filter(|d| seen.contains(d)).collect();
    let track = config.dns.track_domain.trim().to_ascii_lowercase();
    if !track.is_empty() && !track.ends_with(".example.com") && track != "track.postal.example.com"
    {
        plan.track_hostname = Some(track);
    }
    Ok(plan)
}

/// The cached result of the last full run.
#[derive(Default)]
pub struct DnsChecksCache {
    last: tokio::sync::Mutex<Option<(Instant, Value)>>,
}

fn resolver_for(state: &ApiState) -> Arc<dyn DnsCheckResolver> {
    state
        .dns_check_resolver
        .clone()
        .unwrap_or_else(|| Arc::new(crate::dns::HickoryDnsCheckResolver::new()))
}

#[derive(Deserialize)]
pub(crate) struct ChecksParams {
    refresh: Option<String>,
}

/// `GET /api/v2/admin/dns_checks`.
pub(crate) async fn dns_checks_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<ChecksParams>,
) -> ApiResponse {
    let refresh = matches!(params.refresh.as_deref(), Some("1" | "true"));
    let mut cached = state.dns_checks_cache.last.lock().await;
    if !refresh {
        if let Some((at, value)) = cached.as_ref() {
            if at.elapsed() < Duration::from_secs(CACHE_SECONDS) {
                let mut value = value.clone();
                value["cached"] = json!(true);
                return render_success(Some(&start), StatusCode::OK, value);
            }
        }
    }
    let plan = match build_plan(&state).await {
        Ok(plan) => plan,
        Err(error) => return render_store_error(Some(&start), error),
    };
    let resolver = resolver_for(&state);
    let checks = run_checks(resolver.as_ref(), &plan).await;
    let mut value = checks_json(&checks, &plan);
    value["checked_at"] = json!(chrono::Utc::now());
    *cached = Some((Instant::now(), value.clone()));
    value["cached"] = json!(false);
    render_success(Some(&start), StatusCode::OK, value)
}

#[derive(Deserialize)]
pub(crate) struct CandidateParams {
    ipv4: Option<String>,
    ipv6: Option<String>,
    hostname: Option<String>,
}

/// The checks a planned address must pass before it goes into a pool: the
/// A/AAAA of its host name, its PTR, forward-confirmed reverse DNS, the
/// EHLO match (the host name is what it will greet with), and SPF of the
/// return-path domain and the tenant domains.
pub async fn candidate_plan(
    state: &ApiState,
    ipv4: Option<IpAddr>,
    ipv6: Option<IpAddr>,
    hostname: &str,
) -> Result<DnsPlan, camelmailer_core::StoreError> {
    let full = build_plan(state).await?;
    let addresses: Vec<IpAddr> = [ipv4, ipv6].into_iter().flatten().collect();
    Ok(DnsPlan {
        sending: addresses
            .iter()
            .map(|address| SendingAddress {
                address: *address,
                ehlo: hostname.to_ascii_lowercase(),
                origin: "pool",
                pool: None,
            })
            .collect(),
        spf_domains: full.spf_domains,
        truncated_domains: full.truncated_domains,
        ..DnsPlan::default()
    })
}

/// `GET /api/v2/admin/dns_checks/candidate?ipv4=…&ipv6=…&hostname=…`.
pub(crate) async fn dns_checks_candidate(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<CandidateParams>,
) -> ApiResponse {
    let parse = |value: Option<&str>, v4: bool| -> Result<Option<IpAddr>, String> {
        match value.map(str::trim).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(value) => match value.parse::<IpAddr>() {
                Ok(ip) if ip.is_ipv4() == v4 => Ok(Some(ip)),
                _ => Err(format!(
                    "{} must be an {} address",
                    if v4 { "ipv4" } else { "ipv6" },
                    if v4 { "IPv4" } else { "IPv6" }
                )),
            },
        }
    };
    let ipv4 = match parse(params.ipv4.as_deref(), true) {
        Ok(value) => value,
        Err(message) => return render_validation_error(Some(&start), &message),
    };
    let ipv6 = match parse(params.ipv6.as_deref(), false) {
        Ok(value) => value,
        Err(message) => return render_validation_error(Some(&start), &message),
    };
    if ipv4.is_none() && ipv6.is_none() {
        return render_validation_error(Some(&start), "Give an ipv4 or an ipv6 address");
    }
    let hostname = params
        .hostname
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if !camelmailer_core::dns::is_valid_helo_hostname(&hostname) {
        return render_validation_error(
            Some(&start),
            "hostname must be a fully qualified host name (such as out2.mail.example.com)",
        );
    }
    let plan = match candidate_plan(&state, ipv4, ipv6, &hostname).await {
        Ok(plan) => plan,
        Err(error) => return render_store_error(Some(&start), error),
    };
    let resolver = resolver_for(&state);
    let mut checks = Vec::new();
    // The host name's own A/AAAA records come first: the PTR name must
    // resolve to the address, so this is the first record to create.
    let targets: Vec<IpAddr> = [ipv4, ipv6].into_iter().flatten().collect();
    checks.push(
        forward_check(
            resolver.as_ref(),
            "hostname",
            &hostname,
            &targets,
            "planned address",
        )
        .await,
    );
    checks.extend(run_checks(resolver.as_ref(), &plan).await);
    let mut value = checks_json(&checks, &plan);
    value["checked_at"] = json!(chrono::Utc::now());
    value["candidate"] = json!({ "ipv4": ipv4, "ipv6": ipv6, "hostname": hostname });
    value["ready"] = json!(checks.iter().all(|c| c.status == CheckStatus::Ok));
    render_success(Some(&start), StatusCode::OK, value)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A fixed DNS table.
    #[derive(Default, Clone)]
    pub(crate) struct FakeDns {
        pub(crate) txt: HashMap<String, Vec<String>>,
        pub(crate) ips: HashMap<String, Vec<IpAddr>>,
        pub(crate) mx: HashMap<String, Vec<String>>,
        pub(crate) ptr: HashMap<IpAddr, Vec<String>>,
        pub(crate) failing: Vec<String>,
    }

    #[async_trait]
    impl DnsCheckResolver for FakeDns {
        async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
            if self.failing.iter().any(|f| f == name) {
                return Err(DnsError::Lookup("SERVFAIL".into()));
            }
            Ok(self.txt.get(name).cloned().unwrap_or_default())
        }
        async fn ip_addresses(&self, name: &str) -> Result<Vec<IpAddr>, DnsError> {
            if self.failing.iter().any(|f| f == name) {
                return Err(DnsError::Lookup("SERVFAIL".into()));
            }
            Ok(self.ips.get(name).cloned().unwrap_or_default())
        }
        async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError> {
            Ok(self.mx.get(name).cloned().unwrap_or_default())
        }
        async fn ptr(&self, address: IpAddr) -> Result<Vec<String>, DnsError> {
            Ok(self.ptr.get(&address).cloned().unwrap_or_default())
        }
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn sending(address: &str, ehlo: &str) -> SendingAddress {
        SendingAddress {
            address: ip(address),
            ehlo: ehlo.into(),
            origin: "pool",
            pool: Some("Outbound".into()),
        }
    }

    #[test]
    fn instructions_name_the_exact_record() {
        assert_eq!(
            forward_instruction("out1.mail.example.com", &ip("203.0.113.10")),
            "set A out1.mail.example.com → 203.0.113.10, TTL 300"
        );
        assert_eq!(
            forward_instruction("out1.mail.example.com", &ip("2001:db8::10")),
            "set AAAA out1.mail.example.com → 2001:db8::10, TTL 300"
        );
        assert_eq!(
            ptr_instruction(&ip("203.0.113.10"), "out1.mail.example.com"),
            "set PTR 203.0.113.10 → out1.mail.example.com (at the provider of the address)"
        );
    }

    #[test]
    fn spf_mechanisms_go_before_the_all_term() {
        let add = vec![
            "ip4:203.0.113.10".to_string(),
            "ip6:2001:db8::10".to_string(),
        ];
        assert_eq!(
            spf_record_with(Some("v=spf1 include:_spf.example.net -all"), &add),
            "v=spf1 include:_spf.example.net ip4:203.0.113.10 ip6:2001:db8::10 -all"
        );
        assert_eq!(
            spf_record_with(Some("v=spf1 mx redirect=_spf.example.net"), &add[..1]),
            "v=spf1 mx ip4:203.0.113.10 redirect=_spf.example.net"
        );
        assert_eq!(
            spf_record_with(Some("v=spf1 mx"), &add[..1]),
            "v=spf1 mx ip4:203.0.113.10"
        );
        assert_eq!(
            spf_record_with(None, &add[..1]),
            "v=spf1 ip4:203.0.113.10 ~all"
        );
    }

    #[tokio::test]
    async fn a_correct_address_passes_ptr_fcrdns_and_ehlo() {
        let mut dns = FakeDns::default();
        dns.ptr
            .insert(ip("203.0.113.10"), vec!["OUT1.mail.example.com.".into()]);
        dns.ips
            .insert("out1.mail.example.com".into(), vec![ip("203.0.113.10")]);
        let checks = address_checks(&dns, &sending("203.0.113.10", "out1.mail.example.com")).await;
        let kinds: Vec<(&str, CheckStatus)> = checks.iter().map(|c| (c.kind, c.status)).collect();
        assert_eq!(
            kinds,
            vec![
                ("ptr", CheckStatus::Ok),
                ("fcrdns", CheckStatus::Ok),
                ("ehlo", CheckStatus::Ok)
            ]
        );
        assert!(checks.iter().all(|c| c.instructions.is_empty()));
    }

    #[tokio::test]
    async fn a_missing_ptr_asks_for_the_ptr_and_the_forward_record() {
        let dns = FakeDns::default();
        let checks = address_checks(&dns, &sending("2001:db8::10", "out1.mail.example.com")).await;
        assert_eq!(checks[0].kind, "ptr");
        assert_eq!(checks[0].status, CheckStatus::Fail);
        assert_eq!(
            checks[0].instructions,
            vec!["set PTR 2001:db8::10 → out1.mail.example.com (at the provider of the address)"]
        );
        assert_eq!(checks[1].kind, "fcrdns");
        assert_eq!(
            checks[1].instructions,
            vec!["set AAAA out1.mail.example.com → 2001:db8::10, TTL 300"]
        );
        assert_eq!(checks.len(), 2, "no EHLO comparison without a PTR");
    }

    #[tokio::test]
    async fn a_generic_ptr_fails_fcrdns_and_ehlo() {
        let mut dns = FakeDns::default();
        dns.ptr.insert(
            ip("203.0.113.10"),
            vec!["static.10.113.0.203.clients.example.net".into()],
        );
        dns.ips.insert(
            "static.10.113.0.203.clients.example.net".into(),
            vec![ip("198.51.100.1")],
        );
        let checks = address_checks(&dns, &sending("203.0.113.10", "out1.mail.example.com")).await;
        assert_eq!(checks[1].status, CheckStatus::Fail);
        assert_eq!(checks[2].kind, "ehlo");
        assert_eq!(checks[2].status, CheckStatus::Fail);
        assert_eq!(
            checks[2].instructions,
            vec!["set PTR 203.0.113.10 → out1.mail.example.com (at the provider of the address)"]
        );
    }

    #[tokio::test]
    async fn spf_reports_the_mechanisms_to_add() {
        let mut dns = FakeDns::default();
        dns.txt.insert(
            "rp.example.com".into(),
            vec!["v=spf1 ip4:203.0.113.10 -all".into()],
        );
        let domain = SpfDomain {
            name: "rp.example.com".into(),
            role: "return_path",
        };
        let check = spf_check(&dns, &domain, &[ip("203.0.113.10"), ip("203.0.113.20")]).await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(
            check.instructions,
            vec!["set TXT rp.example.com → \"v=spf1 ip4:203.0.113.10 ip4:203.0.113.20 -all\", TTL 300"]
        );
        let check = spf_check(&dns, &domain, &[ip("203.0.113.10")]).await;
        assert_eq!(check.status, CheckStatus::Ok);
    }

    #[tokio::test]
    async fn the_whole_plan_runs_and_groups_by_subject() {
        let mut dns = FakeDns::default();
        dns.ips
            .insert("mail.example.com".into(), vec![ip("198.51.100.5")]);
        dns.ptr
            .insert(ip("203.0.113.10"), vec!["out1.mail.example.com".into()]);
        dns.ips
            .insert("out1.mail.example.com".into(), vec![ip("203.0.113.10")]);
        dns.mx
            .insert("rp.example.com".into(), vec!["mail.example.com".into()]);
        dns.txt.insert(
            "rp.example.com".into(),
            vec!["v=spf1 ip4:203.0.113.10 ~all".into()],
        );
        dns.failing.push("track.example.org".into());
        let plan = DnsPlan {
            inbound_hostname: Some("mail.example.com".into()),
            inbound_targets: vec![ip("198.51.100.5")],
            sending: vec![sending("203.0.113.10", "out1.mail.example.com")],
            spf_domains: vec![SpfDomain {
                name: "rp.example.com".into(),
                role: "return_path",
            }],
            mx_targets: vec!["mail.example.com".into()],
            return_path_domain: Some("rp.example.com".into()),
            inbound_domains: vec!["tenant.example".into()],
            track_hostname: Some("track.example.org".into()),
            truncated_domains: 0,
        };
        let checks = run_checks(&dns, &plan).await;
        let doc = checks_json(&checks, &plan);
        assert_eq!(doc["summary"]["fail"], 0);
        // tenant MX missing: a warning with the record; tracking lookup failed: a warning
        assert_eq!(doc["summary"]["warn"], 2);
        assert_eq!(
            doc["instructions"],
            json!(["set MX tenant.example → 10 mail.example.com, TTL 300"])
        );
        let subjects: Vec<&str> = doc["groups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["subject"].as_str().unwrap())
            .collect();
        assert_eq!(
            subjects,
            vec![
                "mail.example.com",
                "203.0.113.10",
                "rp.example.com",
                "tenant.example",
                "track.example.org"
            ]
        );
        assert_eq!(doc["groups"][1]["checks"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn the_inbound_name_must_point_at_the_load_balancer() {
        let mut dns = FakeDns::default();
        dns.ips
            .insert("mail.example.com".into(), vec![ip("198.51.100.9")]);
        let check = forward_check(
            &dns,
            "hostname",
            "mail.example.com",
            &[ip("198.51.100.5")],
            "load balancer",
        )
        .await;
        assert_eq!(check.status, CheckStatus::Fail);
        assert_eq!(
            check.instructions,
            vec!["set A mail.example.com → 198.51.100.5, TTL 300"]
        );
    }

    #[test]
    fn pool_addresses_greet_with_their_hostname_only_when_configured() {
        let mut config = camelmailer_config::Config::default();
        config.dns.helo_hostname = Some("mx.example.com".into());
        assert_eq!(pool_ehlo(&config, "out1.example.com"), "mx.example.com");
        config.smtp_client.helo_from_ip_address = true;
        assert_eq!(pool_ehlo(&config, "out1.example.com"), "out1.example.com");
        assert_eq!(pool_ehlo(&config, "not valid"), "mx.example.com");
    }
}
