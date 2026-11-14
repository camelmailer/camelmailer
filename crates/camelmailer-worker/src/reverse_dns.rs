//! Reverse-DNS check of the addresses a worker sends from.
//!
//! Large receivers refuse mail from an address without forward-confirmed
//! reverse DNS (FCrDNS): Microsoft 365 answers `550 5.7.1 ... S820` to an
//! IPv6 sender without a PTR record. The worker therefore checks, at start
//! and every `smtp_client.reverse_dns_check_interval` seconds, every address
//! it may send from:
//!
//! - each IPv4 and IPv6 value of every IP pool address (`origin: pool`);
//! - the IPv4 and IPv6 address this host sends from when a server has no
//!   pool (`origin: host`), found by asking the kernel which local address
//!   it would use towards a public address (a UDP `connect`, which sends
//!   nothing).
//!
//! An address passes when it has a PTR record and one of the PTR names
//! resolves back to the address. Every other result is logged as a warning,
//! stored in `source_address_checks` (read by
//! `GET /api/v2/admin/source_addresses`) and kept in [`ReverseDnsState`],
//! from which [`crate::SmtpSender`] can leave out a failing family
//! (`smtp_client.skip_family_without_reverse_dns`).

use crate::smtp_client::{AddressFamily, OutboundSource};
use async_trait::async_trait;
use camelmailer_core::{AdminStore, SourceAddressCheck};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

/// The result of checking one address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReverseDnsStatus {
    /// A PTR name resolves back to the address.
    Pass,
    /// The address has no PTR record.
    NoPtr,
    /// The address has PTR records, but none of their names resolves back
    /// to it.
    Mismatch,
    /// A lookup failed (timeout, SERVFAIL); decided at the next check.
    LookupFailed,
    /// A private, loopback or link-local address: receivers see the address
    /// of the NAT or load balancer in front of the host, which the worker
    /// cannot know.
    NotPublic,
}

impl ReverseDnsStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReverseDnsStatus::Pass => "pass",
            ReverseDnsStatus::NoPtr => "no_ptr",
            ReverseDnsStatus::Mismatch => "mismatch",
            ReverseDnsStatus::LookupFailed => "lookup_failed",
            ReverseDnsStatus::NotPublic => "not_public",
        }
    }

    /// Whether receivers would see this address without valid reverse DNS.
    /// Undecided results (lookup failures, non-public addresses) are not
    /// failures.
    pub fn failed(self) -> bool {
        matches!(self, ReverseDnsStatus::NoPtr | ReverseDnsStatus::Mismatch)
    }
}

/// Where a checked address comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// What this host sends from for servers without an IP pool.
    Host,
    /// An IP pool address.
    Pool,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Host => "host",
            Origin::Pool => "pool",
        }
    }
}

/// The check of one address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddressCheck {
    pub address: IpAddr,
    pub origin: Origin,
    pub status: ReverseDnsStatus,
    /// The PTR names found.
    pub ptr: Vec<String>,
    /// A sentence for the log and the admin API.
    pub detail: String,
}

/// A failed DNS lookup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LookupError {
    /// The name exists but has no record of the type, or does not exist.
    NotFound,
    /// Anything else (timeout, SERVFAIL, …).
    Failed(String),
}

/// The two lookups the check needs; tests substitute a fixed table.
#[async_trait]
pub trait ReverseDnsResolver: Send + Sync {
    /// The PTR names of `address`, without the trailing dot.
    async fn ptr(&self, address: IpAddr) -> Result<Vec<String>, LookupError>;
    /// The addresses `name` resolves to, of the same family as `family_of`.
    async fn addresses(&self, name: &str, family_of: IpAddr) -> Result<Vec<IpAddr>, LookupError>;
}

/// The production resolver: hickory with the system configuration.
pub struct HickoryReverseDnsResolver {
    resolver: hickory_resolver::TokioResolver,
}

impl HickoryReverseDnsResolver {
    pub fn new() -> Result<Self, String> {
        let resolver = hickory_resolver::TokioResolver::builder_tokio()
            .and_then(|builder| builder.build())
            .map_err(|error| error.to_string())?;
        Ok(Self { resolver })
    }
}

fn lookup_error(error: hickory_resolver::net::NetError) -> LookupError {
    if error.is_no_records_found() {
        LookupError::NotFound
    } else {
        LookupError::Failed(error.to_string())
    }
}

#[async_trait]
impl ReverseDnsResolver for HickoryReverseDnsResolver {
    async fn ptr(&self, address: IpAddr) -> Result<Vec<String>, LookupError> {
        use hickory_resolver::proto::rr::{Name, RData};
        let lookup = self
            .resolver
            .reverse_lookup(Name::from(address))
            .await
            .map_err(lookup_error)?;
        Ok(lookup
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::PTR(name) => Some(name.0.to_utf8().trim_end_matches('.').to_string()),
                _ => None,
            })
            .collect())
    }

    async fn addresses(&self, name: &str, family_of: IpAddr) -> Result<Vec<IpAddr>, LookupError> {
        use hickory_resolver::proto::rr::RData;
        let fqdn = format!("{}.", name.trim_end_matches('.'));
        let lookup = if family_of.is_ipv4() {
            self.resolver.ipv4_lookup(fqdn).await
        } else {
            self.resolver.ipv6_lookup(fqdn).await
        }
        .map_err(lookup_error)?;
        Ok(lookup
            .answers()
            .iter()
            .filter_map(|record| match &record.data {
                RData::A(a) => Some(IpAddr::V4(a.0)),
                RData::AAAA(aaaa) => Some(IpAddr::V6(aaaa.0)),
                _ => None,
            })
            .collect())
    }
}

/// Whether receivers could see `address` as the sender: not private,
/// loopback, link-local, unspecified, multicast or (IPv4) carrier-grade NAT.
pub fn is_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public_v4(v4),
            None => {
                let first = v6.segments()[0];
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    || (first & 0xfe00) == 0xfc00 // unique local fc00::/7
                    || (first & 0xffc0) == 0xfe80) // link-local fe80::/10
            }
        },
    }
}

fn is_public_v4(v4: Ipv4Addr) -> bool {
    let [a, b, ..] = v4.octets();
    !(v4.is_private()
        || v4.is_loopback()
        || v4.is_link_local()
        || v4.is_unspecified()
        || v4.is_broadcast()
        || v4.is_multicast()
        || (a == 100 && (64..128).contains(&b))) // carrier-grade NAT 100.64/10
}

/// Check one address: a PTR record whose name resolves back to it.
pub async fn check_address(
    resolver: &dyn ReverseDnsResolver,
    address: IpAddr,
    origin: Origin,
) -> AddressCheck {
    let result = |status, ptr: Vec<String>, detail: String| AddressCheck {
        address,
        origin,
        status,
        ptr,
        detail,
    };
    if !is_public(address) {
        return result(
            ReverseDnsStatus::NotPublic,
            Vec::new(),
            format!(
                "{address} is not a public address; receivers see the address of the NAT \
                 or load balancer in front of this host, which needs the PTR record"
            ),
        );
    }
    let names = match resolver.ptr(address).await {
        Ok(names) if !names.is_empty() => names,
        Ok(_) | Err(LookupError::NotFound) => {
            return result(
                ReverseDnsStatus::NoPtr,
                Vec::new(),
                format!(
                    "{address} has no PTR record; receivers such as Microsoft 365 refuse \
                     mail from it (S820)"
                ),
            )
        }
        Err(LookupError::Failed(error)) => {
            return result(
                ReverseDnsStatus::LookupFailed,
                Vec::new(),
                format!("the PTR lookup of {address} failed: {error}"),
            )
        }
    };
    let mut failure = None;
    for name in &names {
        match resolver.addresses(name, address).await {
            Ok(resolved) if resolved.contains(&address) => {
                return result(
                    ReverseDnsStatus::Pass,
                    names.clone(),
                    format!("{address} has the PTR record {name}, which resolves back to it"),
                )
            }
            Ok(_) | Err(LookupError::NotFound) => {}
            Err(LookupError::Failed(error)) => failure = Some(error),
        }
    }
    match failure {
        Some(error) => result(
            ReverseDnsStatus::LookupFailed,
            names,
            format!("the forward lookup of the PTR name of {address} failed: {error}"),
        ),
        None => {
            let detail = format!(
                "{address} has the PTR record {}, but it does not resolve back to {address}; \
                 add the matching {} record",
                names.join(", "),
                if address.is_ipv4() { "A" } else { "AAAA" }
            );
            result(ReverseDnsStatus::Mismatch, names, detail)
        }
    }
}

/// Public addresses the kernel's default routes lead to. Nothing is sent:
/// a UDP `connect` only picks the local address. These are a.root-servers.net,
/// which stay routed and stable.
const PROBE_V4: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(198, 41, 0, 4)), 53);
const PROBE_V6: SocketAddr = SocketAddr::new(
    IpAddr::V6(Ipv6Addr::new(0x2001, 0x503, 0xba3e, 0, 0, 0, 0x2, 0x30)),
    53,
);

/// The local IPv4 and IPv6 address this host sends from when the operating
/// system picks it (a server without an IP pool). `None` for a family the
/// host has no route for.
pub fn host_addresses() -> (Option<IpAddr>, Option<IpAddr>) {
    let local = |bind: SocketAddr, probe: SocketAddr| {
        let socket = std::net::UdpSocket::bind(bind).ok()?;
        socket.connect(probe).ok()?;
        Some(socket.local_addr().ok()?.ip())
    };
    (
        local(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0), PROBE_V4),
        local(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0), PROBE_V6),
    )
}

/// The latest results, shared between the periodic check and the sender.
#[derive(Debug, Default)]
pub struct ReverseDnsState {
    inner: RwLock<Snapshot>,
}

#[derive(Debug, Default, Clone)]
struct Snapshot {
    results: HashMap<IpAddr, ReverseDnsStatus>,
    host_ipv4: Option<IpAddr>,
    host_ipv6: Option<IpAddr>,
}

impl ReverseDnsState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The state of this process, filled by [`spawn`] and read by every
    /// [`crate::SmtpSender`] built with [`crate::SmtpSender::new`]. Empty
    /// (nothing ever failed) until the first check has run.
    pub fn global() -> Arc<Self> {
        static GLOBAL: OnceLock<Arc<ReverseDnsState>> = OnceLock::new();
        GLOBAL.get_or_init(ReverseDnsState::new).clone()
    }

    /// Replace the results with one check run.
    pub fn update(
        &self,
        checks: &[AddressCheck],
        host_ipv4: Option<IpAddr>,
        host_ipv6: Option<IpAddr>,
    ) {
        let snapshot = Snapshot {
            results: checks
                .iter()
                .map(|check| (check.address, check.status))
                .collect(),
            host_ipv4,
            host_ipv6,
        };
        *self.inner.write().expect("reverse DNS state") = snapshot;
    }

    /// The latest result for `address`, if it was checked.
    pub fn status(&self, address: IpAddr) -> Option<ReverseDnsStatus> {
        self.inner
            .read()
            .expect("reverse DNS state")
            .results
            .get(&address)
            .copied()
    }

    /// `source` without a family whose source address failed the check,
    /// when the other family's passed or is undecided: `dual_stack` and
    /// `prefer_ipv4` become `ipv4_only` or `ipv6_only`. The source addresses
    /// are the pool's for a pool, else the host's. Nothing changes when both
    /// fail, when a family has no source address, or for `ipv4_only` and
    /// `ipv6_only`, which the operator chose.
    pub fn narrow(&self, source: OutboundSource) -> OutboundSource {
        if !matches!(
            source.family,
            AddressFamily::DualStack | AddressFamily::PreferIpv4
        ) {
            return source;
        }
        let snapshot = self.inner.read().expect("reverse DNS state").clone();
        let (v4, v6) = if source.pinned {
            (source.ipv4.map(IpAddr::V4), source.ipv6.map(IpAddr::V6))
        } else {
            (snapshot.host_ipv4, snapshot.host_ipv6)
        };
        let (Some(v4), Some(v6)) = (v4, v6) else {
            return source;
        };
        let failed = |ip: IpAddr| snapshot.results.get(&ip).is_some_and(|s| s.failed());
        let family = match (failed(v4), failed(v6)) {
            (false, true) => AddressFamily::Ipv4Only,
            (true, false) => AddressFamily::Ipv6Only,
            _ => return source,
        };
        tracing::debug!(
            ipv4 = %v4,
            ipv6 = %v6,
            %family,
            "leaving out the address family whose source failed the reverse-DNS check"
        );
        OutboundSource { family, ..source }
    }
}

/// Every address to check: the host's (for servers without a pool) and both
/// values of every pool address, each once (a pool address wins over the
/// same host address).
pub async fn addresses_to_check(
    store: &dyn AdminStore,
    host_ipv4: Option<IpAddr>,
    host_ipv6: Option<IpAddr>,
) -> Vec<(IpAddr, Origin)> {
    let mut targets: Vec<(IpAddr, Origin)> = Vec::new();
    let mut add = |address: IpAddr, origin: Origin| {
        if let Some(existing) = targets.iter_mut().find(|(a, _)| *a == address) {
            if origin == Origin::Pool {
                existing.1 = Origin::Pool;
            }
        } else {
            targets.push((address, origin));
        }
    };
    match store.list_ip_pools().await {
        Ok(pools) => {
            for pool in pools {
                match store.list_ip_addresses(pool.id).await {
                    Ok(addresses) => {
                        for address in addresses {
                            let values = std::iter::once(Some(address.ipv4.as_str()))
                                .chain(std::iter::once(address.ipv6.as_deref()));
                            for value in values.flatten() {
                                match value.trim().parse::<IpAddr>() {
                                    Ok(ip) => add(ip, Origin::Pool),
                                    Err(_) if value.trim().is_empty() => {}
                                    Err(_) => tracing::warn!(
                                        pool = %pool.name,
                                        value,
                                        "IP pool address is not an IP address; it is never bound"
                                    ),
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, pool = %pool.name, "could not list the addresses of an IP pool")
                    }
                }
            }
        }
        Err(error) => tracing::warn!(%error, "could not list the IP pools"),
    }
    for address in [host_ipv4, host_ipv6].into_iter().flatten() {
        add(address, Origin::Host);
    }
    targets
}

/// One check run: check every address, log each failure, store the results
/// and update `state`. Returns the checks.
pub async fn run_once(
    resolver: &dyn ReverseDnsResolver,
    store: &dyn AdminStore,
    state: &ReverseDnsState,
    host_ipv4: Option<IpAddr>,
    host_ipv6: Option<IpAddr>,
    checked_by: &str,
) -> Vec<AddressCheck> {
    let targets = addresses_to_check(store, host_ipv4, host_ipv6).await;
    let mut checks = Vec::with_capacity(targets.len());
    for (address, origin) in targets {
        let check = check_address(resolver, address, origin).await;
        match check.status {
            ReverseDnsStatus::Pass => {
                tracing::info!(%address, origin = origin.as_str(), ptr = ?check.ptr, "source address has forward-confirmed reverse DNS")
            }
            status if status.failed() => {
                let remedy = match origin {
                    Origin::Pool => "set the PTR record at the provider of the address, or set the pool's address_family to avoid this family",
                    Origin::Host => "this is the address servers without an IP pool send from; set the PTR record at the provider of the address, or set smtp_client.address_family to avoid this family",
                };
                tracing::warn!(%address, origin = origin.as_str(), status = status.as_str(), "{}; {remedy}", check.detail)
            }
            status => {
                tracing::info!(%address, origin = origin.as_str(), status = status.as_str(), "{}", check.detail)
            }
        }
        checks.push(check);
    }
    state.update(&checks, host_ipv4, host_ipv6);
    let now = chrono::Utc::now();
    let rows: Vec<SourceAddressCheck> = checks
        .iter()
        .map(|check| SourceAddressCheck {
            address: check.address.to_string(),
            family: if check.address.is_ipv4() {
                "ipv4"
            } else {
                "ipv6"
            }
            .into(),
            origin: check.origin.as_str().into(),
            status: check.status.as_str().into(),
            ptr: check.ptr.clone(),
            detail: check.detail.clone(),
            checked_by: checked_by.into(),
            checked_at: now,
        })
        .collect();
    if let Err(error) = store.record_source_address_checks(&rows).await {
        tracing::warn!(%error, "could not store the reverse-DNS results");
    }
    checks
}

/// This host's name, for `checked_by`.
fn host_name() -> String {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "worker".into())
}

/// Run the check at start and then every
/// `smtp_client.reverse_dns_check_interval` seconds, filling
/// [`ReverseDnsState::global`]. `None` when the interval is `0`.
pub fn spawn(
    config: &camelmailer_config::Config,
    store: Arc<dyn AdminStore>,
) -> Option<tokio::task::JoinHandle<()>> {
    let interval = config.smtp_client.reverse_dns_check_interval;
    if interval == 0 {
        tracing::info!("reverse-DNS check of the source addresses is off");
        return None;
    }
    let resolver = match HickoryReverseDnsResolver::new() {
        Ok(resolver) => resolver,
        Err(error) => {
            tracing::warn!(%error, "no resolver for the reverse-DNS check of the source addresses");
            return None;
        }
    };
    let state = ReverseDnsState::global();
    let checked_by = host_name();
    Some(tokio::spawn(async move {
        loop {
            let (host_ipv4, host_ipv6) = host_addresses();
            run_once(
                &resolver,
                store.as_ref(),
                &state,
                host_ipv4,
                host_ipv6,
                &checked_by,
            )
            .await;
            tokio::time::sleep(Duration::from_secs(u64::from(interval))).await;
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use camelmailer_core::admin_store::NewIpAddress;
    use std::collections::HashMap;

    /// A fixed DNS table.
    #[derive(Default)]
    struct Table {
        ptr: HashMap<IpAddr, Result<Vec<String>, LookupError>>,
        forward: HashMap<String, Result<Vec<IpAddr>, LookupError>>,
    }

    #[async_trait]
    impl ReverseDnsResolver for Table {
        async fn ptr(&self, address: IpAddr) -> Result<Vec<String>, LookupError> {
            self.ptr
                .get(&address)
                .cloned()
                .unwrap_or(Err(LookupError::NotFound))
        }

        async fn addresses(
            &self,
            name: &str,
            family_of: IpAddr,
        ) -> Result<Vec<IpAddr>, LookupError> {
            self.forward
                .get(name)
                .cloned()
                .unwrap_or(Err(LookupError::NotFound))
                .map(|all| {
                    all.into_iter()
                        .filter(|a| a.is_ipv4() == family_of.is_ipv4())
                        .collect()
                })
        }
    }

    fn ip(value: &str) -> IpAddr {
        value.parse().unwrap()
    }

    fn table() -> Table {
        let mut table = Table::default();
        // 203.0.113.10 <-> out.example.com: forward-confirmed
        table
            .ptr
            .insert(ip("203.0.113.10"), Ok(vec!["out.example.com".into()]));
        table.forward.insert(
            "out.example.com".into(),
            Ok(vec![ip("203.0.113.10"), ip("2001:db8::10")]),
        );
        table
            .ptr
            .insert(ip("2001:db8::10"), Ok(vec!["out.example.com".into()]));
        // 203.0.113.20 -> generic.isp.example, which resolves elsewhere
        table
            .ptr
            .insert(ip("203.0.113.20"), Ok(vec!["generic.isp.example".into()]));
        table
            .forward
            .insert("generic.isp.example".into(), Ok(vec![ip("203.0.113.99")]));
        // 203.0.113.30: the resolver is down
        table.ptr.insert(
            ip("203.0.113.30"),
            Err(LookupError::Failed("SERVFAIL".into())),
        );
        // 2001:db8::20 has no PTR (not in the table)
        table
    }

    #[tokio::test]
    async fn a_ptr_that_resolves_back_passes() {
        let check = check_address(&table(), ip("203.0.113.10"), Origin::Pool).await;
        assert_eq!(check.status, ReverseDnsStatus::Pass);
        assert_eq!(check.ptr, vec!["out.example.com".to_string()]);

        // the AAAA lookup confirms an IPv6 address
        let check = check_address(&table(), ip("2001:db8::10"), Origin::Host).await;
        assert_eq!(check.status, ReverseDnsStatus::Pass);
    }

    #[tokio::test]
    async fn no_ptr_mismatch_and_lookup_failures_are_told_apart() {
        let check = check_address(&table(), ip("2001:db8::20"), Origin::Host).await;
        assert_eq!(check.status, ReverseDnsStatus::NoPtr);
        assert!(check.detail.contains("S820"), "{}", check.detail);
        assert!(check.status.failed());

        let check = check_address(&table(), ip("203.0.113.20"), Origin::Pool).await;
        assert_eq!(check.status, ReverseDnsStatus::Mismatch);
        assert!(check.detail.contains("generic.isp.example"));
        assert!(check.detail.contains("add the matching A record"));
        assert!(check.status.failed());

        let check = check_address(&table(), ip("203.0.113.30"), Origin::Pool).await;
        assert_eq!(check.status, ReverseDnsStatus::LookupFailed);
        assert!(!check.status.failed(), "undecided is not a failure");
    }

    #[tokio::test]
    async fn private_addresses_are_not_looked_up() {
        for address in [
            "10.0.0.5",
            "192.168.1.2",
            "100.64.0.1",
            "fd00::5",
            "fe80::1",
            "::1",
        ] {
            let check = check_address(&table(), ip(address), Origin::Host).await;
            assert_eq!(check.status, ReverseDnsStatus::NotPublic, "{address}");
        }
        assert!(is_public(ip("203.0.113.10")));
        assert!(is_public(ip("2a01:4f8::1")));
        assert!(!is_public(ip("::ffff:10.0.0.1")));
    }

    #[tokio::test]
    async fn a_run_checks_pool_and_host_addresses_and_stores_the_results() {
        let store = camelmailer_core::MemoryStore::new();
        let pool = store.create_ip_pool("main", false).await.unwrap();
        store
            .create_ip_address(NewIpAddress {
                ip_pool_id: pool.id,
                ipv4: "203.0.113.10".into(),
                ipv6: Some("2001:db8::10".into()),
                hostname: "out.example.com".into(),
                priority: 10,
            })
            .await
            .unwrap();
        store
            .create_ip_address(NewIpAddress {
                ip_pool_id: pool.id,
                ipv4: "203.0.113.20".into(),
                ipv6: None,
                hostname: "two.example.com".into(),
                priority: 20,
            })
            .await
            .unwrap();
        let state = ReverseDnsState::new();
        // The host sends from a pool address over IPv4 and from an IPv6
        // address without a PTR.
        let checks = run_once(
            &table(),
            &store,
            &state,
            Some(ip("203.0.113.10")),
            Some(ip("2001:db8::20")),
            "worker-1",
        )
        .await;
        let summary: Vec<(String, Origin, ReverseDnsStatus)> = checks
            .iter()
            .map(|c| (c.address.to_string(), c.origin, c.status))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("203.0.113.10".into(), Origin::Pool, ReverseDnsStatus::Pass),
                ("2001:db8::10".into(), Origin::Pool, ReverseDnsStatus::Pass),
                (
                    "203.0.113.20".into(),
                    Origin::Pool,
                    ReverseDnsStatus::Mismatch
                ),
                ("2001:db8::20".into(), Origin::Host, ReverseDnsStatus::NoPtr),
            ]
        );
        assert_eq!(
            state.status(ip("2001:db8::20")),
            Some(ReverseDnsStatus::NoPtr)
        );

        let stored = store.list_source_address_checks().await.unwrap();
        assert_eq!(stored.len(), 4);
        // failures first, then by address
        assert_eq!(stored[0].address, "2001:db8::20");
        assert_eq!(stored[0].status, "no_ptr");
        assert_eq!(stored[0].family, "ipv6");
        assert_eq!(stored[0].origin, "host");
        assert_eq!(stored[0].checked_by, "worker-1");
        assert_eq!(stored[1].address, "203.0.113.20");
        assert_eq!(stored[1].status, "mismatch");
        assert_eq!(stored[1].family, "ipv4");
        assert_eq!(stored[1].origin, "pool");
    }

    #[test]
    fn narrow_leaves_out_the_failing_family_of_the_host() {
        let state = ReverseDnsState::new();
        let host_v4 = ip("203.0.113.10");
        let host_v6 = ip("2001:db8::20");
        let check = |address: IpAddr, status| AddressCheck {
            address,
            origin: Origin::Host,
            status,
            ptr: Vec::new(),
            detail: String::new(),
        };
        state.update(
            &[
                check(host_v4, ReverseDnsStatus::Pass),
                check(host_v6, ReverseDnsStatus::NoPtr),
            ],
            Some(host_v4),
            Some(host_v6),
        );
        let narrowed = state.narrow(OutboundSource::system(AddressFamily::DualStack));
        assert_eq!(narrowed.family, AddressFamily::Ipv4Only);
        let narrowed = state.narrow(OutboundSource::system(AddressFamily::PreferIpv4));
        assert_eq!(narrowed.family, AddressFamily::Ipv4Only);
        // an explicit choice stays
        let narrowed = state.narrow(OutboundSource::system(AddressFamily::Ipv6Only));
        assert_eq!(narrowed.family, AddressFamily::Ipv6Only);

        // both failing: nothing to gain by leaving one out
        state.update(
            &[
                check(host_v4, ReverseDnsStatus::Mismatch),
                check(host_v6, ReverseDnsStatus::NoPtr),
            ],
            Some(host_v4),
            Some(host_v6),
        );
        let narrowed = state.narrow(OutboundSource::system(AddressFamily::DualStack));
        assert_eq!(narrowed.family, AddressFamily::DualStack);

        // an undecided IPv4 result still lets IPv6 be left out
        state.update(
            &[
                check(host_v4, ReverseDnsStatus::LookupFailed),
                check(host_v6, ReverseDnsStatus::NoPtr),
            ],
            Some(host_v4),
            Some(host_v6),
        );
        let narrowed = state.narrow(OutboundSource::system(AddressFamily::DualStack));
        assert_eq!(narrowed.family, AddressFamily::Ipv4Only);

        // a host without IPv6 has nothing to leave out
        state.update(
            &[check(host_v4, ReverseDnsStatus::NoPtr)],
            Some(host_v4),
            None,
        );
        let narrowed = state.narrow(OutboundSource::system(AddressFamily::DualStack));
        assert_eq!(narrowed.family, AddressFamily::DualStack);
    }

    #[test]
    fn narrow_uses_the_pool_addresses_for_a_pool() {
        let state = ReverseDnsState::new();
        let check = |address: &str, status| AddressCheck {
            address: ip(address),
            origin: Origin::Pool,
            status,
            ptr: Vec::new(),
            detail: String::new(),
        };
        // the host itself is fine, the pool's IPv4 is not
        state.update(
            &[
                check("203.0.113.20", ReverseDnsStatus::Mismatch),
                check("2001:db8::10", ReverseDnsStatus::Pass),
            ],
            Some(ip("192.0.2.1")),
            None,
        );
        let source = OutboundSource::pool(
            AddressFamily::DualStack,
            Some("203.0.113.20".parse().unwrap()),
            Some("2001:db8::10".parse().unwrap()),
        );
        assert_eq!(state.narrow(source).family, AddressFamily::Ipv6Only);
    }
}
