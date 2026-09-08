//! Inbound SPF (Sender Policy Framework, RFC 7208) evaluation.
//!
//! For a received message we look up the envelope-From (MAIL FROM) domain's
//! SPF record and evaluate it against the connecting client IP, producing one
//! of the standard results (pass / fail / softfail / neutral / none /
//! temperror / permerror) plus a ready-to-prepend `Received-SPF:` header
//! value.
//!
//! This is deliberately a *bounded, non-macro* evaluator: it covers the
//! mechanisms that matter in practice — `all`, `ip4`, `ip6`, `a`, `mx`,
//! `include` and the `redirect=` modifier — with the RFC's 10-DNS-lookup
//! cap to stop loops, but it does not implement the full macro (`%{…}`)
//! expansion language. That is enough to record an authoritative-enough
//! result as an informational header without turning the receive path into a
//! DNS amplifier.
//!
//! DNS access goes through the [`SpfResolver`] trait so the whole thing is
//! unit-testable with the in-memory [`StaticSpfResolver`], exactly like the
//! TXT-only [`crate::dns::DnsResolver`] used by domain verification.

use crate::dns::DnsError;
use async_trait::async_trait;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};

/// RFC 7208 §2.6 result of an SPF check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpfResult {
    Pass,
    Fail,
    SoftFail,
    Neutral,
    None,
    TempError,
    PermError,
}

impl SpfResult {
    /// The lowercase keyword used in the `Received-SPF:` header (RFC 7208 §9.1).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::SoftFail => "softfail",
            Self::Neutral => "neutral",
            Self::None => "none",
            Self::TempError => "temperror",
            Self::PermError => "permerror",
        }
    }

    /// The qualifier characters map to results: `+`pass `-`fail `~`softfail
    /// `?`neutral.
    fn from_qualifier(qualifier: char) -> Self {
        match qualifier {
            '-' => Self::Fail,
            '~' => Self::SoftFail,
            '?' => Self::Neutral,
            _ => Self::Pass,
        }
    }
}

/// A term this bounded evaluator cannot resolve, and therefore the reason a
/// verdict is not trustworthy. Named so a caller can tell an operator what
/// stood in the way rather than reporting a misleading result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedTerm {
    /// The term as published, e.g. `include:%{ir}.%{v}.%{d}.spf.has.pphosted.com`.
    pub term: String,
    /// The record that carried it, which may be a nested `include` target
    /// rather than the domain that was asked about.
    pub domain: String,
    /// Which unsupported construct it is.
    pub kind: UnsupportedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedKind {
    /// RFC 7208 §7 macro expansion (`%{i}`, `%{ir}`, `%{d}`, …). Hosted SPF
    /// services build per-sender includes this way (Proofpoint's
    /// `%{ir}.%{v}.%{d}.spf.has.pphosted.com`), so the target name only
    /// exists once expanded against a specific sending IP.
    Macro,
    /// `exists:` — matches on the existence of a name that is almost always
    /// macro-derived.
    Exists,
    /// `ptr` — deprecated by RFC 7208 §5.5 and requires reverse DNS.
    Ptr,
}

impl UnsupportedKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Macro => "macro",
            Self::Exists => "exists",
            Self::Ptr => "ptr",
        }
    }
}

/// The outcome of an evaluation that reports its own trustworthiness.
///
/// [`evaluate`] answers with a bare [`SpfResult`] because the receive path
/// only needs something to stamp into a header. A caller that turns the
/// answer into advice ("your DNS does not authorize us") needs to know when
/// the evaluator was unable to decide, because a confidently wrong
/// instruction is worse than none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpfVerdict {
    /// The record was fully evaluated with the mechanisms this evaluator
    /// supports; the result is the answer a receiver would reach.
    Determined(SpfResult),
    /// Evaluation reached a term it cannot resolve and the outcome depends on
    /// it, so no result is reported. Verify by other means.
    Unverifiable(UnsupportedTerm),
}

/// Does `term` use RFC 7208 macro expansion?
fn term_uses_macro(term: &str) -> bool {
    term.contains("%{")
}

/// Classify a term this evaluator cannot resolve, if it is one. Checked
/// before any DNS is spent, so a macro-built name is never queried literally.
fn unsupported_kind(term: &str) -> Option<UnsupportedKind> {
    if term_uses_macro(term) {
        return Some(UnsupportedKind::Macro);
    }
    let lower = term.to_ascii_lowercase();
    if lower.starts_with("exists:") {
        return Some(UnsupportedKind::Exists);
    }
    if lower == "ptr" || lower.starts_with("ptr:") {
        return Some(UnsupportedKind::Ptr);
    }
    None
}

/// The DNS a full SPF evaluation needs: TXT (the record itself, `include`,
/// `redirect`), plus A/AAAA (`a`) and MX (`mx`). Errors mean *lookup failed*
/// (→ temperror), distinct from "no records" (`Ok(vec![])`).
#[async_trait]
pub trait SpfResolver: Send + Sync {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError>;
    /// A + AAAA addresses published for `name`.
    async fn ip_addresses(&self, name: &str) -> Result<Vec<IpAddr>, DnsError>;
    /// MX exchange hostnames published for `name` (any preference order).
    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError>;
}

/// The RFC 7208 §4.6.4 processing limit: at most 10 DNS-querying mechanisms
/// / modifiers per evaluation, to bound work and stop include/redirect loops.
const MAX_DNS_LOOKUPS: u32 = 10;

/// Evaluate the SPF policy of `domain` for a message arriving from
/// `client_ip`. Never blocks the caller's decision — it only returns the
/// result for the caller to record.
pub async fn evaluate(resolver: &dyn SpfResolver, domain: &str, client_ip: IpAddr) -> SpfResult {
    if domain.trim().is_empty() {
        return SpfResult::None;
    }
    let evaluator = Evaluator {
        resolver,
        budget: AtomicU32::new(MAX_DNS_LOOKUPS),
        macro_mode: MacroMode::Lenient,
        blocked: std::sync::Mutex::new(None),
    };
    evaluator.check_host(domain.to_string(), client_ip).await
}

/// Evaluate `domain`'s SPF against `client_ip` and say whether the answer can
/// be trusted.
///
/// Same walk as [`evaluate`], with one difference: a term this evaluator
/// cannot resolve (a macro, `exists:`, `ptr`) is recorded instead of being
/// silently treated as a non-match. If nothing matched and such a term was
/// consulted, the outcome depends on it and the verdict is
/// [`SpfVerdict::Unverifiable`] rather than a `fail` or `neutral` that would
/// be reported to an operator as fact.
///
/// A `pass` is always [`SpfVerdict::Determined`]: the match came from a term
/// that was resolved, so an unresolvable term later in the record cannot
/// change it.
///
/// Use this wherever a result becomes advice. [`evaluate`] remains the entry
/// point for the receive path, whose behaviour this does not alter.
pub async fn evaluate_verifiable(
    resolver: &dyn SpfResolver,
    domain: &str,
    client_ip: IpAddr,
) -> SpfVerdict {
    if domain.trim().is_empty() {
        return SpfVerdict::Determined(SpfResult::None);
    }
    let evaluator = Evaluator {
        resolver,
        budget: AtomicU32::new(MAX_DNS_LOOKUPS),
        macro_mode: MacroMode::Report,
        blocked: std::sync::Mutex::new(None),
    };
    let result = evaluator.check_host(domain.to_string(), client_ip).await;
    let blocked = evaluator.blocked.lock().unwrap().clone();
    match (result, blocked) {
        (SpfResult::Pass, _) => SpfVerdict::Determined(SpfResult::Pass),
        (_, Some(term)) => SpfVerdict::Unverifiable(term),
        (result, None) => SpfVerdict::Determined(result),
    }
}

/// Build the value of a `Received-SPF:` header (without the field name, to
/// match [`crate::received_header::generate`]). Prepended to a stored inbound
/// message so operators and downstream filters can see the verdict.
pub fn received_spf_header(
    result: SpfResult,
    receiver: &str,
    envelope_from: &str,
    client_ip: IpAddr,
    helo: &str,
) -> String {
    let comment = match result {
        SpfResult::Pass => format!("{receiver}: domain of {envelope_from} designates {client_ip} as permitted sender"),
        SpfResult::Fail => format!("{receiver}: domain of {envelope_from} does not designate {client_ip} as permitted sender"),
        SpfResult::SoftFail => format!("{receiver}: domain of transitioning {envelope_from} does not designate {client_ip} as permitted sender"),
        SpfResult::Neutral => format!("{receiver}: {client_ip} is neither permitted nor denied by domain of {envelope_from}"),
        SpfResult::None => format!("{receiver}: {envelope_from} does not publish an SPF policy"),
        SpfResult::TempError => format!("{receiver}: transient error resolving SPF for {envelope_from}"),
        SpfResult::PermError => format!("{receiver}: permanent error evaluating SPF for {envelope_from}"),
    };
    let mut header = format!(
        "{} ({}) client-ip={}; envelope-from=<{}>;",
        result.as_str(),
        comment,
        client_ip,
        envelope_from
    );
    if !helo.is_empty() {
        header.push_str(&format!(" helo={helo};"));
    }
    header
}

/// Whether the walk reports terms it cannot resolve, or keeps the receive
/// path's lenient behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MacroMode {
    /// [`evaluate`]: unsupported terms take the paths they always took, so
    /// the inbound verdict is unchanged. A macro `include:` still resolves
    /// its literal name, finds no record and yields the permerror it does
    /// today.
    Lenient,
    /// [`evaluate_verifiable`]: an unsupported term is recorded and skipped
    /// without spending DNS, so the caller can say "not verifiable" instead
    /// of reporting a result that depends on a term nobody evaluated.
    Report,
}

struct Evaluator<'a> {
    resolver: &'a dyn SpfResolver,
    budget: AtomicU32,
    macro_mode: MacroMode,
    /// The first unresolvable term actually consulted during the walk. Only
    /// set in [`MacroMode::Report`].
    blocked: std::sync::Mutex<Option<UnsupportedTerm>>,
}

impl<'a> Evaluator<'a> {
    /// Spend one unit of the DNS-lookup budget; `false` once it is exhausted.
    fn spend_lookup(&self) -> bool {
        self.budget
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                remaining.checked_sub(1)
            })
            .is_ok()
    }

    /// Evaluate `domain`'s SPF record against `ip`. Boxed so `include` /
    /// `redirect` can recurse.
    fn check_host<'s>(
        &'s self,
        domain: String,
        ip: IpAddr,
    ) -> Pin<Box<dyn Future<Output = SpfResult> + Send + 's>> {
        Box::pin(async move {
            let record = match self.fetch_spf_record(&domain).await {
                Ok(Some(record)) => record,
                Ok(None) => return SpfResult::None,
                Err(result) => return result,
            };

            let mut redirect: Option<String> = None;
            for term in record.split_whitespace().skip(1) {
                // modifiers (name=value)
                if let Some(target) = term.strip_prefix("redirect=") {
                    if self.macro_mode == MacroMode::Report && term_uses_macro(target) {
                        self.note_unsupported(term, &domain, UnsupportedKind::Macro);
                        continue;
                    }
                    redirect = Some(target.to_string());
                    continue;
                }
                if term.contains('=') {
                    // unknown modifier (e.g. exp=) — ignore
                    continue;
                }

                let (qualifier, mechanism) = split_qualifier(term);
                match self.match_mechanism(mechanism, &domain, ip).await {
                    MechanismOutcome::Matched => return SpfResult::from_qualifier(qualifier),
                    MechanismOutcome::NoMatch => {}
                    MechanismOutcome::Error(result) => return result,
                }
            }

            // No mechanism matched: a `redirect=` modifier, if present, is the
            // authoritative fallback (RFC 7208 §6.1). Otherwise the default
            // result is neutral.
            match redirect {
                Some(target) => {
                    if !self.spend_lookup() {
                        return SpfResult::PermError;
                    }
                    self.check_host(target, ip).await
                }
                None => SpfResult::Neutral,
            }
        })
    }

    /// Fetch the single `v=spf1` TXT record for `domain`. `Ok(None)` = no
    /// policy; `Err(_)` carries the SPF result to short-circuit with
    /// (temperror on lookup failure, permerror on multiple records).
    async fn fetch_spf_record(&self, domain: &str) -> Result<Option<String>, SpfResult> {
        if !self.spend_lookup() {
            return Err(SpfResult::PermError);
        }
        let records = match self.resolver.txt(domain).await {
            Ok(records) => records,
            Err(_) => return Err(SpfResult::TempError),
        };
        let mut spf: Vec<String> = records
            .into_iter()
            .filter(|r| {
                let head = r.trim_start();
                head == "v=spf1" || head.starts_with("v=spf1 ")
            })
            .collect();
        match spf.len() {
            0 => Ok(None),
            1 => Ok(Some(spf.pop().unwrap())),
            // Multiple SPF records is a permanent configuration error.
            _ => Err(SpfResult::PermError),
        }
    }

    /// Record the first unresolvable term consulted, so the verdict can say
    /// what stood in the way. Only the first is kept: it is the one that
    /// explains the outcome, and a list would just be noise.
    fn note_unsupported(&self, term: &str, domain: &str, kind: UnsupportedKind) {
        let mut blocked = self.blocked.lock().unwrap();
        if blocked.is_none() {
            *blocked = Some(UnsupportedTerm {
                term: term.to_string(),
                domain: domain.to_string(),
                kind,
            });
        }
    }

    async fn match_mechanism(
        &self,
        mechanism: &str,
        current_domain: &str,
        ip: IpAddr,
    ) -> MechanismOutcome {
        // In reporting mode an unresolvable term is noted and skipped before
        // any DNS is spent, so a macro-built name is never queried literally.
        // Lenient mode falls through to the paths it always took, leaving the
        // receive path's verdict untouched.
        if self.macro_mode == MacroMode::Report {
            if let Some(kind) = unsupported_kind(mechanism) {
                self.note_unsupported(mechanism, current_domain, kind);
                return MechanismOutcome::NoMatch;
            }
        }
        let lower = mechanism.to_ascii_lowercase();
        if lower == "all" {
            return MechanismOutcome::Matched;
        }
        if let Some(rest) = lower
            .strip_prefix("ip4:")
            .or_else(|| lower.strip_prefix("ip6:"))
        {
            return match ip_in_cidr(rest, ip) {
                Some(true) => MechanismOutcome::Matched,
                Some(false) => MechanismOutcome::NoMatch,
                None => MechanismOutcome::Error(SpfResult::PermError),
            };
        }
        if lower == "a" || lower.starts_with("a:") || lower.starts_with("a/") {
            return self.match_a(mechanism, current_domain, ip).await;
        }
        if lower == "mx" || lower.starts_with("mx:") || lower.starts_with("mx/") {
            return self.match_mx(mechanism, current_domain, ip).await;
        }
        if let Some(target) = lower.strip_prefix("include:") {
            return self.match_include(target, ip).await;
        }
        // ptr/exists and unknown mechanisms: unsupported, treated as no-match
        // (we never PermError on them, keeping evaluation lenient/bounded).
        MechanismOutcome::NoMatch
    }

    async fn match_a(&self, mechanism: &str, current_domain: &str, ip: IpAddr) -> MechanismOutcome {
        let (target, cidr) = parse_domain_spec(mechanism, "a", current_domain);
        if !self.spend_lookup() {
            return MechanismOutcome::Error(SpfResult::PermError);
        }
        let addresses = match self.resolver.ip_addresses(&target).await {
            Ok(addresses) => addresses,
            Err(_) => return MechanismOutcome::Error(SpfResult::TempError),
        };
        if addresses.iter().any(|addr| ip_matches(*addr, ip, cidr)) {
            MechanismOutcome::Matched
        } else {
            MechanismOutcome::NoMatch
        }
    }

    async fn match_mx(
        &self,
        mechanism: &str,
        current_domain: &str,
        ip: IpAddr,
    ) -> MechanismOutcome {
        let (target, cidr) = parse_domain_spec(mechanism, "mx", current_domain);
        if !self.spend_lookup() {
            return MechanismOutcome::Error(SpfResult::PermError);
        }
        let hosts = match self.resolver.mx_hosts(&target).await {
            Ok(hosts) => hosts,
            Err(_) => return MechanismOutcome::Error(SpfResult::TempError),
        };
        for host in hosts {
            if !self.spend_lookup() {
                return MechanismOutcome::Error(SpfResult::PermError);
            }
            match self.resolver.ip_addresses(&host).await {
                Ok(addresses) => {
                    if addresses.iter().any(|addr| ip_matches(*addr, ip, cidr)) {
                        return MechanismOutcome::Matched;
                    }
                }
                Err(_) => return MechanismOutcome::Error(SpfResult::TempError),
            }
        }
        MechanismOutcome::NoMatch
    }

    async fn match_include(&self, target: &str, ip: IpAddr) -> MechanismOutcome {
        if !self.spend_lookup() {
            return MechanismOutcome::Error(SpfResult::PermError);
        }
        // RFC 7208 §5.2: include matches on Pass; None/error propagate as a
        // permerror/temperror; Fail/SoftFail/Neutral are a non-match.
        match self.check_host(target.to_string(), ip).await {
            SpfResult::Pass => MechanismOutcome::Matched,
            SpfResult::Fail | SpfResult::SoftFail | SpfResult::Neutral => MechanismOutcome::NoMatch,
            SpfResult::None => MechanismOutcome::Error(SpfResult::PermError),
            SpfResult::TempError => MechanismOutcome::Error(SpfResult::TempError),
            SpfResult::PermError => MechanismOutcome::Error(SpfResult::PermError),
        }
    }
}

enum MechanismOutcome {
    Matched,
    NoMatch,
    Error(SpfResult),
}

/// Split a term's leading qualifier (`+`/`-`/`~`/`?`, default `+`) from the
/// mechanism text.
fn split_qualifier(term: &str) -> (char, &str) {
    match term.chars().next() {
        Some(q @ ('+' | '-' | '~' | '?')) => (q, &term[1..]),
        _ => ('+', term),
    }
}

/// Parse the `domain[/cidr]` part of an `a` / `mx` mechanism. `keyword` is
/// `"a"` or `"mx"`; a missing domain defaults to the current domain, a
/// missing cidr to a full-length host match.
fn parse_domain_spec(mechanism: &str, keyword: &str, current_domain: &str) -> (String, Option<u8>) {
    // strip the keyword (case-insensitively) from the front
    let rest = &mechanism[keyword.len().min(mechanism.len())..];
    let (domain_part, cidr) = match rest.split_once('/') {
        Some((domain, cidr)) => (domain, cidr.parse::<u8>().ok()),
        None => (rest, None),
    };
    let domain = match domain_part.strip_prefix(':') {
        Some(explicit) if !explicit.is_empty() => explicit.to_string(),
        _ => current_domain.to_string(),
    };
    (domain, cidr)
}

/// Does `ip` fall inside the `addr[/len]` CIDR string? `None` on a malformed
/// spec.
fn ip_in_cidr(spec: &str, ip: IpAddr) -> Option<bool> {
    if let Ok(net) = spec.parse::<ipnet::IpNet>() {
        return Some(net.contains(&ip));
    }
    // bare address without a prefix length
    let addr = spec.parse::<IpAddr>().ok()?;
    Some(addr == ip)
}

/// Does a resolved address `record` match the connecting `ip`, honouring an
/// optional `a`/`mx` CIDR prefix length?
fn ip_matches(record: IpAddr, ip: IpAddr, cidr: Option<u8>) -> bool {
    match cidr {
        None => record == ip,
        Some(len) => match ipnet::IpNet::new(record, len) {
            Ok(net) => net.contains(&ip),
            Err(_) => record == ip,
        },
    }
}

/// In-memory [`SpfResolver`] for tests: name → TXT / A+AAAA / MX maps, with an
/// optional forced lookup failure (to exercise temperror).
#[derive(Default)]
pub struct StaticSpfResolver {
    txt: std::sync::RwLock<std::collections::HashMap<String, Vec<String>>>,
    addresses: std::sync::RwLock<std::collections::HashMap<String, Vec<IpAddr>>>,
    mx: std::sync::RwLock<std::collections::HashMap<String, Vec<String>>>,
    fail: std::sync::RwLock<bool>,
}

impl StaticSpfResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_txt(&self, name: &str, value: &str) {
        self.txt
            .write()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .push(value.to_string());
    }

    pub fn add_address(&self, name: &str, ip: IpAddr) {
        self.addresses
            .write()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .push(ip);
    }

    pub fn add_mx(&self, name: &str, host: &str) {
        self.mx
            .write()
            .unwrap()
            .entry(name.to_string())
            .or_default()
            .push(host.to_string());
    }

    /// Make every subsequent lookup fail (→ temperror).
    pub fn fail_all(&self) {
        *self.fail.write().unwrap() = true;
    }
}

#[async_trait]
impl SpfResolver for StaticSpfResolver {
    async fn txt(&self, name: &str) -> Result<Vec<String>, DnsError> {
        if *self.fail.read().unwrap() {
            return Err(DnsError::Lookup("forced failure".into()));
        }
        Ok(self
            .txt
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default())
    }

    async fn ip_addresses(&self, name: &str) -> Result<Vec<IpAddr>, DnsError> {
        if *self.fail.read().unwrap() {
            return Err(DnsError::Lookup("forced failure".into()));
        }
        Ok(self
            .addresses
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default())
    }

    async fn mx_hosts(&self, name: &str) -> Result<Vec<String>, DnsError> {
        if *self.fail.read().unwrap() {
            return Err(DnsError::Lookup("forced failure".into()));
        }
        Ok(self
            .mx
            .read()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[tokio::test]
    async fn ip4_pass_and_fail() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("acme.com", "v=spf1 ip4:192.0.2.0/24 -all");

        // an IP inside the permitted block passes
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("192.0.2.15")).await,
            SpfResult::Pass
        );
        // an IP outside it hits `-all` → fail
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("198.51.100.1")).await,
            SpfResult::Fail
        );
    }

    #[tokio::test]
    async fn softfail_and_neutral_defaults() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("soft.example", "v=spf1 ip4:203.0.113.0/24 ~all");
        assert_eq!(
            evaluate(&resolver, "soft.example", ip("192.0.2.1")).await,
            SpfResult::SoftFail
        );

        // no `all` and nothing matched → neutral
        let bare = StaticSpfResolver::new();
        bare.add_txt("bare.example", "v=spf1 ip4:203.0.113.0/24");
        assert_eq!(
            evaluate(&bare, "bare.example", ip("192.0.2.1")).await,
            SpfResult::Neutral
        );
    }

    #[tokio::test]
    async fn no_policy_is_none_and_lookup_failure_is_temperror() {
        let resolver = StaticSpfResolver::new();
        assert_eq!(
            evaluate(&resolver, "unknown.example", ip("192.0.2.1")).await,
            SpfResult::None
        );

        resolver.add_txt("acme.com", "v=spf1 -all");
        resolver.fail_all();
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("192.0.2.1")).await,
            SpfResult::TempError
        );
    }

    #[tokio::test]
    async fn a_and_mx_mechanisms() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("acme.com", "v=spf1 a mx -all");
        resolver.add_address("acme.com", ip("192.0.2.10"));
        resolver.add_mx("acme.com", "mail.acme.com");
        resolver.add_address("mail.acme.com", ip("198.51.100.20"));

        // matches the `a` record
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("192.0.2.10")).await,
            SpfResult::Pass
        );
        // matches an MX host's address
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("198.51.100.20")).await,
            SpfResult::Pass
        );
        // neither → fail via -all
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("203.0.113.9")).await,
            SpfResult::Fail
        );
    }

    #[tokio::test]
    async fn include_matches_on_pass_only() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("acme.com", "v=spf1 include:_spf.provider.net -all");
        resolver.add_txt("_spf.provider.net", "v=spf1 ip4:192.0.2.0/24 ~all");

        // the included policy passes the IP → include matches → pass
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("192.0.2.5")).await,
            SpfResult::Pass
        );
        // the included policy does not pass this IP → include is a no-match,
        // so evaluation falls through to the parent's -all → fail
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("198.51.100.5")).await,
            SpfResult::Fail
        );
    }

    #[tokio::test]
    async fn redirect_modifier_is_the_fallback() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("acme.com", "v=spf1 redirect=_spf.acme.com");
        resolver.add_txt("_spf.acme.com", "v=spf1 ip4:192.0.2.0/24 -all");
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("192.0.2.7")).await,
            SpfResult::Pass
        );
        assert_eq!(
            evaluate(&resolver, "acme.com", ip("10.0.0.1")).await,
            SpfResult::Fail
        );
    }

    #[tokio::test]
    async fn lookup_loops_are_bounded_to_permerror() {
        let resolver = StaticSpfResolver::new();
        // a.example includes b.example includes a.example …
        resolver.add_txt("a.example", "v=spf1 include:b.example -all");
        resolver.add_txt("b.example", "v=spf1 include:a.example -all");
        assert_eq!(
            evaluate(&resolver, "a.example", ip("192.0.2.1")).await,
            SpfResult::PermError
        );
    }

    #[test]
    fn received_spf_header_reports_the_result() {
        let header = received_spf_header(
            SpfResult::Pass,
            "mail.example.net",
            "user@acme.com",
            ip("192.0.2.10"),
            "helo.acme.com",
        );
        assert!(header.starts_with("pass ("));
        assert!(header.contains("client-ip=192.0.2.10;"));
        assert!(header.contains("envelope-from=<user@acme.com>;"));
        assert!(header.contains("helo=helo.acme.com;"));

        let fail = received_spf_header(
            SpfResult::Fail,
            "mail.example.net",
            "user@acme.com",
            ip("192.0.2.10"),
            "",
        );
        assert!(fail.starts_with("fail ("));
        assert!(!fail.contains("helo="));
    }
}

#[cfg(test)]
mod verifiable_tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    /// Proofpoint's hosted SPF, the shape that made a literal-match health
    /// check tell operators to add an include they may not need.
    const HOSTED: &str = "v=spf1 include:%{ir}.%{v}.%{d}.spf.has.pphosted.com ~all";

    #[tokio::test]
    async fn a_resolvable_record_is_determined() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("acme.com", "v=spf1 ip4:192.0.2.0/24 -all");

        assert_eq!(
            evaluate_verifiable(&resolver, "acme.com", ip("192.0.2.15")).await,
            SpfVerdict::Determined(SpfResult::Pass)
        );
        assert_eq!(
            evaluate_verifiable(&resolver, "acme.com", ip("198.51.100.1")).await,
            SpfVerdict::Determined(SpfResult::Fail)
        );
    }

    #[tokio::test]
    async fn a_macro_include_is_unverifiable_rather_than_a_fail() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("hosted.example", HOSTED);

        match evaluate_verifiable(&resolver, "hosted.example", ip("192.0.2.1")).await {
            SpfVerdict::Unverifiable(term) => {
                assert_eq!(term.kind, UnsupportedKind::Macro);
                assert_eq!(term.domain, "hosted.example");
                assert!(term.term.contains("%{ir}"), "{}", term.term);
            }
            other => panic!("expected Unverifiable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn evaluate_keeps_its_receive_path_behaviour_for_the_same_record() {
        // The bare entry point must not change: a macro include still
        // resolves its literal name, finds no record, and permerrors exactly
        // as it did before evaluate_verifiable existed.
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("hosted.example", HOSTED);
        assert_eq!(
            evaluate(&resolver, "hosted.example", ip("192.0.2.1")).await,
            SpfResult::PermError
        );
    }

    #[tokio::test]
    async fn a_macro_nested_in_an_include_is_reported_with_that_domain() {
        // The domain asked about is clean; the provider it delegates to is
        // the one using macros. The report names the nested record.
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("clean.example", "v=spf1 include:provider.example -all");
        resolver.add_txt("provider.example", HOSTED);

        match evaluate_verifiable(&resolver, "clean.example", ip("192.0.2.1")).await {
            SpfVerdict::Unverifiable(term) => {
                assert_eq!(term.kind, UnsupportedKind::Macro);
                assert_eq!(term.domain, "provider.example");
            }
            other => panic!("expected Unverifiable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_pass_before_the_macro_stays_determined() {
        // The match came from a term that was resolved, so an unresolvable
        // term further along cannot change the answer.
        let resolver = StaticSpfResolver::new();
        resolver.add_txt(
            "mixed.example",
            "v=spf1 ip4:192.0.2.0/24 include:%{ir}.spf.example ~all",
        );
        assert_eq!(
            evaluate_verifiable(&resolver, "mixed.example", ip("192.0.2.9")).await,
            SpfVerdict::Determined(SpfResult::Pass)
        );
    }

    #[tokio::test]
    async fn the_same_record_is_unverifiable_for_an_ip_that_reaches_the_macro() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt(
            "mixed.example",
            "v=spf1 ip4:192.0.2.0/24 include:%{ir}.spf.example ~all",
        );
        assert!(matches!(
            evaluate_verifiable(&resolver, "mixed.example", ip("198.51.100.7")).await,
            SpfVerdict::Unverifiable(_)
        ));
    }

    #[tokio::test]
    async fn exists_and_ptr_are_reported_too() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("e.example", "v=spf1 exists:%{i}._spf.e.example -all");
        resolver.add_txt("p.example", "v=spf1 ptr -all");

        match evaluate_verifiable(&resolver, "e.example", ip("192.0.2.1")).await {
            // The macro check runs first, which is the honest label here:
            // the name is macro-derived.
            SpfVerdict::Unverifiable(term) => assert_eq!(term.kind, UnsupportedKind::Macro),
            other => panic!("expected Unverifiable, got {other:?}"),
        }
        match evaluate_verifiable(&resolver, "p.example", ip("192.0.2.1")).await {
            SpfVerdict::Unverifiable(term) => assert_eq!(term.kind, UnsupportedKind::Ptr),
            other => panic!("expected Unverifiable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_redirect_with_a_macro_target_is_reported() {
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("r.example", "v=spf1 redirect=%{d}.spf.example");
        match evaluate_verifiable(&resolver, "r.example", ip("192.0.2.1")).await {
            SpfVerdict::Unverifiable(term) => {
                assert_eq!(term.kind, UnsupportedKind::Macro);
                assert!(term.term.starts_with("redirect="), "{}", term.term);
            }
            other => panic!("expected Unverifiable, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_nested_include_chain_resolves_without_a_literal_match() {
        // The point of using the evaluator at all: our mechanism is not in
        // the domain's record, it is two includes deep, and the answer is
        // still pass.
        let resolver = StaticSpfResolver::new();
        resolver.add_txt("tenant.example", "v=spf1 include:reseller.example -all");
        resolver.add_txt(
            "reseller.example",
            "v=spf1 include:spf.camelmailer.test ~all",
        );
        resolver.add_txt("spf.camelmailer.test", "v=spf1 ip4:203.0.113.0/24 -all");

        assert_eq!(
            evaluate_verifiable(&resolver, "tenant.example", ip("203.0.113.10")).await,
            SpfVerdict::Determined(SpfResult::Pass)
        );
        assert_eq!(
            evaluate_verifiable(&resolver, "tenant.example", ip("198.51.100.1")).await,
            SpfVerdict::Determined(SpfResult::Fail)
        );
    }

    #[tokio::test]
    async fn no_record_is_determined_none() {
        let resolver = StaticSpfResolver::new();
        assert_eq!(
            evaluate_verifiable(&resolver, "nothing.example", ip("192.0.2.1")).await,
            SpfVerdict::Determined(SpfResult::None)
        );
    }
}
