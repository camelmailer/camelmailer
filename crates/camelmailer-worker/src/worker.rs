//! The message dequeuer — the port of `app/lib/message_dequeuer` plus the
//! webhook dispatch of `app/models/webhook_request.rb`.
//!
//! The worker owns the cross-tenant queue but enters each message's tenant
//! context (via the RLS-aware accessors on `camelmailer-db`) to read
//! content, so tenant isolation never depends on worker code being careful.

use crate::dkim;
use crate::inspection::{ClamavInspector, RspamdInspector};
use crate::sender::SmtpSender;
use crate::signer::Signer;
use crate::smtp_client::{DataGate, SendOutcome};
use crate::ssrf::SsrfGuard;
use crate::tracking;
use base64::Engine;
use camelmailer_core::{AdminStore, RouteMode};
use camelmailer_core::{DeliveryKind, DeliveryStatsStore, IpDeliveryRecord};
use camelmailer_db::webhook_payload::{
    postal_webhook_message_id, postal_webhook_subject, postal_webhook_timestamp,
};
use camelmailer_db::{
    BounceCorrelationOutcome, BounceOriginal, FeedbackComplaintOutcome, OutboundQueueAction,
    OutboundRecord, PgLifecycleQueue, PgMessageSink, PgQueue, PgStore, PgWebhookQueue, QueueFilter,
    StoredMessage,
};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Webhook deliveries are retried this many times before giving up
/// (mirrors Postal's webhook retry schedule length).
const WEBHOOK_MAX_ATTEMPTS: i32 = 10;

/// How often the worker loop runs housekeeping.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(3600);

/// How long a host-aware worker trusts its view of which pool addresses it
/// holds, and of which pools have one.
const LOCAL_ADDRESS_TTL: Duration = Duration::from_secs(30);

/// FNV-1a over the lowercased recipient domain: a hash that stays the same
/// across processes, hosts and releases, so one receiving domain keeps
/// seeing the same address of a host.
fn domain_hash(domain: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in domain.trim().to_ascii_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

/// Pick the source among the pool addresses this host holds: the lowest
/// priority number, ties spread by the recipient domain. `addresses` must be
/// ordered by priority, then id (as the store returns them).
pub fn choose_source_address<'a>(
    addresses: &[&'a camelmailer_core::IpAddress],
    recipient_domain: &str,
) -> Option<&'a camelmailer_core::IpAddress> {
    let best = addresses.iter().map(|a| a.priority).min()?;
    let tied: Vec<&camelmailer_core::IpAddress> = addresses
        .iter()
        .copied()
        .filter(|a| a.priority == best)
        .collect();
    let index = (domain_hash(recipient_domain) % tied.len() as u64) as usize;
    Some(tied[index])
}

/// The IP addresses configured on this host's network interfaces.
///
/// Read from the interface list rather than by trying to bind each address:
/// with `net.ipv4.ip_nonlocal_bind=1` (common with keepalived and floating
/// IPs) or `IP_FREEBIND` every bind succeeds, so a bind probe would call every
/// pool address local.
#[cfg(unix)]
fn interface_addresses() -> std::io::Result<HashSet<IpAddr>> {
    let addresses = nix::ifaddrs::getifaddrs().map_err(std::io::Error::from)?;
    Ok(addresses
        .filter_map(|interface| interface.address)
        .filter_map(|address| {
            if let Some(v4) = address.as_sockaddr_in() {
                Some(IpAddr::V4(v4.ip()))
            } else {
                address.as_sockaddr_in6().map(|v6| IpAddr::V6(v6.ip()))
            }
        })
        .collect())
}

/// Without an interface list on this platform, nothing counts as local:
/// list the addresses in `worker.source_addresses` instead.
#[cfg(not(unix))]
fn interface_addresses() -> std::io::Result<HashSet<IpAddr>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no interface list on this platform; set worker.source_addresses",
    ))
}

/// The EHLO name of a pool address (`smtp_client.helo_from_ip_address`):
/// its `hostname` when that is a fully qualified host name, else `None`, and
/// the worker greets with `dns.helo_hostname`. The API only stores valid
/// names; this guards rows written before that check or around the API, so a
/// CR, LF or space never reaches the SMTP command line.
fn pool_helo_hostname(address: &camelmailer_core::IpAddress) -> Option<&str> {
    let name = address.hostname.as_str();
    if name.is_empty() {
        return None;
    }
    if camelmailer_core::is_valid_helo_hostname(name) {
        return Some(name);
    }
    tracing::warn!(
        ip_address_id = address.id,
        hostname = ?name,
        "pool address hostname is not a fully qualified host name; \
         greeting with dns.helo_hostname instead"
    );
    None
}

/// Which pool addresses this host may send from (`worker.host_aware_pools`).
struct SourceSelection {
    /// `worker.source_addresses`; `None` takes the interface addresses.
    explicit: Option<HashSet<IpAddr>>,
    /// The interface addresses, with when they were read.
    discovered: Mutex<Option<(Arc<HashSet<IpAddr>>, Instant)>>,
    /// When the queue's pool filter was last refreshed.
    pools_refreshed: Mutex<Option<Instant>>,
    /// Pools reported as having no address here. Cleared for a pool once the
    /// host holds one of its addresses again, so a later loss is logged too.
    reported: Mutex<HashSet<camelmailer_core::Id>>,
    foreign_pool_delay_seconds: i64,
}

impl SourceSelection {
    fn new(settings: &camelmailer_config::Worker) -> Self {
        let explicit = (!settings.source_addresses.is_empty()).then(|| {
            settings
                .source_addresses
                .iter()
                .filter_map(|address| address.parse().ok())
                .collect()
        });
        Self {
            explicit,
            discovered: Mutex::default(),
            pools_refreshed: Mutex::default(),
            reported: Mutex::default(),
            foreign_pool_delay_seconds: i64::from(settings.foreign_pool_delay_seconds.max(1)),
        }
    }

    /// The interface addresses, re-read at most every 30 s. The lock is not
    /// held while the list is read.
    fn discovered(&self) -> Arc<HashSet<IpAddr>> {
        if let Some((addresses, at)) = &*self.discovered.lock().expect("address lock") {
            if at.elapsed() < LOCAL_ADDRESS_TTL {
                return Arc::clone(addresses);
            }
        }
        let addresses = Arc::new(interface_addresses().unwrap_or_else(|error| {
            tracing::warn!(%error, "could not read the interface addresses");
            HashSet::new()
        }));
        *self.discovered.lock().expect("address lock") =
            Some((Arc::clone(&addresses), Instant::now()));
        addresses
    }

    /// A check for "this host can send from that address": listed in
    /// `worker.source_addresses`, or (without a list) configured on one of
    /// this host's interfaces. Take it once per batch of lookups.
    fn local_check(&self) -> impl Fn(&str) -> bool + '_ {
        let discovered = self.explicit.is_none().then(|| self.discovered());
        move |address: &str| {
            let Ok(ip) = address.trim().parse::<IpAddr>() else {
                return false;
            };
            match (&self.explicit, &discovered) {
                (Some(explicit), _) => explicit.contains(&ip),
                (None, Some(discovered)) => discovered.contains(&ip),
                (None, None) => false,
            }
        }
    }

    /// Log once per pool that this host holds none of its addresses.
    fn report_foreign(&self, pool_id: camelmailer_core::Id) {
        if self.reported.lock().expect("report lock").insert(pool_id) {
            tracing::info!(
                pool_id,
                "this host holds no address of IP pool; leaving its mail to the hosts that do"
            );
        }
    }

    /// Forget the reports of pools this host holds an address of again.
    fn clear_reports(&self, local_pools: &[i64]) {
        self.reported
            .lock()
            .expect("report lock")
            .retain(|pool| !local_pools.contains(&(*pool as i64)));
    }
}

#[cfg(test)]
mod interface_tests {
    use super::*;

    #[test]
    fn the_loopback_address_is_an_interface_address() {
        let addresses = interface_addresses().unwrap();
        assert!(
            addresses.contains(&IpAddr::from([127, 0, 0, 1])),
            "{addresses:?}"
        );
    }

    /// A bind probe calls an address local when the bind succeeds, which it
    /// always does for the unspecified address (and, with
    /// `ip_nonlocal_bind`, for every address). The interface list does not.
    #[test]
    fn a_bindable_address_that_is_on_no_interface_is_not_local() {
        assert!(std::net::UdpSocket::bind("0.0.0.0:0").is_ok());
        let selection = SourceSelection::new(&camelmailer_config::Worker::default());
        let is_local = selection.local_check();
        assert!(!is_local("0.0.0.0"));
        assert!(!is_local("192.0.2.1"));
        assert!(is_local("127.0.0.1"));
    }

    #[test]
    fn a_listed_address_replaces_the_interface_list() {
        let settings = camelmailer_config::Worker {
            source_addresses: vec!["198.51.100.7".into()],
            ..Default::default()
        };
        let selection = SourceSelection::new(&settings);
        let is_local = selection.local_check();
        assert!(is_local("198.51.100.7"));
        assert!(!is_local("127.0.0.1"));
    }

    #[test]
    fn a_pool_is_reported_again_after_the_host_held_it_in_between() {
        let selection = SourceSelection::new(&camelmailer_config::Worker::default());
        selection.report_foreign(7);
        assert!(selection.reported.lock().unwrap().contains(&7));
        selection.clear_reports(&[7]);
        assert!(!selection.reported.lock().unwrap().contains(&7));
    }
}

/// How long the per-address delivery counters are kept (IP metrics,
/// warm-up history).
const DELIVERY_STATS_DAYS: i64 = 400;

/// How long the daily warm-up usage is kept.
const WARMUP_USAGE_DAYS: i64 = 30;

/// How long the worker remembers which address sent an accepted message, so
/// a feedback-loop complaint can be counted against it. Feedback loops
/// report within hours to a few days.
const MESSAGE_SOURCE_DAYS: i64 = 30;

/// How often a worker refreshes the locks of the rows it is working on. The
/// shortest lease the queues accept is three of these.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// How far the processing of one in-flight row has got.
#[derive(Debug, Clone)]
enum Phase {
    /// Loading and preparing; nothing has reached a receiver yet.
    Preparing,
    /// An outgoing message on its way to the receiving MX or relay. Until
    /// its [`DataGate`] is entered (the client is about to send `DATA`) no
    /// receiver can have the message, and a shutdown may hand the row back.
    Delivering(DataGate),
    /// Talking to the receiver (SMTP from `DATA` on, a route, webhook or
    /// lifecycle URL). From here on the receiver may already have the
    /// message, so a stopped row keeps its lock for the lease instead of
    /// going straight back to the queue.
    Sending,
    /// Handed back to the queue by a shutdown; must not reach a receiver.
    Released,
}

/// Warn when a queue change matched no row because this worker no longer
/// holds it (its lease ran out and another worker took it over).
fn owned(table: &str, id: i64, matched: bool) {
    if !matched {
        tracing::warn!(
            table,
            id,
            "another worker took over this row before its result was recorded; left to that worker"
        );
    }
}

/// Lock a mutex even when a panicking thread poisoned it: the sets below
/// stay consistent across a panic, and the heartbeat must keep running.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The rows of one queue this process is working on right now. A row is in
/// here from its dequeue until its processing returns, whatever the outcome,
/// so the heartbeat only keeps alive what is really in flight. A row whose
/// processing failed half way drops out and is taken over by another worker
/// once its lease runs out; it is never handed back early.
#[derive(Default)]
struct Tracked {
    rows: Mutex<HashMap<i64, Phase>>,
    /// Rows whose processing panicked, waiting to be deferred.
    panicked: Mutex<Vec<i64>>,
}

impl Tracked {
    fn hold(&self, id: i64) -> Held<'_> {
        lock(&self.rows).insert(id, Phase::Preparing);
        Held { tracked: self, id }
    }

    /// Every row in flight, for the heartbeat.
    fn ids(&self) -> Vec<i64> {
        lock(&self.rows).keys().copied().collect()
    }

    /// Take the rows in flight that have not reached a receiver yet, the
    /// only ones that may go back to the queue on shutdown: rows still
    /// preparing, and outgoing deliveries whose client has not sent `DATA`.
    /// Their gates are closed in the same step, so none of them can send its
    /// message afterwards, and they are marked released.
    fn take_unstarted(&self) -> Vec<i64> {
        let mut rows = lock(&self.rows);
        let mut taken = Vec::new();
        for (id, phase) in rows.iter_mut() {
            let unstarted = match phase {
                Phase::Preparing => true,
                Phase::Delivering(gate) => gate.close(),
                Phase::Sending | Phase::Released => false,
            };
            if unstarted {
                *phase = Phase::Released;
                taken.push(*id);
            }
        }
        taken
    }

    /// Row `id` is about to reach its receiver.
    fn sending(&self, id: i64) {
        if let Some(phase) = lock(&self.rows).get_mut(&id) {
            if !matches!(phase, Phase::Released) {
                *phase = Phase::Sending;
            }
        }
    }

    /// Outgoing row `id` is about to be delivered over SMTP. Returns the
    /// gate its client enters before `DATA`; for a row a shutdown already
    /// handed back the gate is closed, so its message never goes out.
    fn delivering(&self, id: i64) -> DataGate {
        let gate = DataGate::default();
        match lock(&self.rows).get_mut(&id) {
            Some(Phase::Released) => {
                gate.close();
            }
            Some(phase) => *phase = Phase::Delivering(gate.clone()),
            None => {}
        }
        gate
    }

    fn contains(&self, id: i64) -> bool {
        lock(&self.rows).contains_key(&id)
    }

    fn take_panicked(&self) -> Vec<i64> {
        std::mem::take(&mut *lock(&self.panicked))
    }
}

/// The rows this process is working on, per queue.
#[derive(Default)]
struct InFlight {
    messages: Tracked,
    webhooks: Tracked,
    lifecycle: Tracked,
}

/// Marks one row as in flight for as long as it lives.
struct Held<'a> {
    tracked: &'a Tracked,
    id: i64,
}

impl Held<'_> {
    /// The row is about to reach its receiver.
    fn sending(&self) {
        self.tracked.sending(self.id);
    }
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        lock(&self.tracked.rows).remove(&self.id);
        // Dropped while unwinding: the processing of this row panicked.
        if std::thread::panicking() {
            lock(&self.tracked.panicked).push(self.id);
        }
    }
}

/// Run `future`, turning a panic inside it into an error carrying the panic
/// message, so one bad row cannot take a worker loop down.
async fn catch_panic<F: Future>(future: F) -> Result<F::Output, String> {
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(move |cx| {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| future.as_mut().poll(cx))) {
            Ok(std::task::Poll::Ready(output)) => std::task::Poll::Ready(Ok(output)),
            Ok(std::task::Poll::Pending) => std::task::Poll::Pending,
            Err(payload) => std::task::Poll::Ready(Err(panic_message(payload.as_ref()))),
        }
    })
    .await
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "panic with a non-string payload".to_string()
    }
}

/// A worker loop ended on its own, which only a bug can cause. The worker
/// stops with this error so the process manager restarts it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopFailed(pub String);

impl std::fmt::Display for LoopFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "worker stopped: {}", self.0)
    }
}

impl std::error::Error for LoopFailed {}

/// Rows a worker handed back to the queue on shutdown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Released {
    pub messages: u64,
    pub webhooks: u64,
    pub lifecycle_events: u64,
}

/// Resolves on SIGTERM or SIGINT (Ctrl-C). Installing the handler also makes
/// the worker stoppable when it runs as PID 1 in a container without an init.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = tokio::signal::ctrl_c() => {}
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not listen for SIGTERM; only SIGINT stops the worker gracefully");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Sleep for `duration`, or less when `stop` turns true.
async fn pause(stop: &mut tokio::sync::watch::Receiver<bool>, duration: Duration) {
    if *stop.borrow() {
        return;
    }
    tokio::select! {
        _ = tokio::time::sleep(duration) => {}
        _ = stop.changed() => {}
    }
}

const UNMATCHED_BOUNCE_DETAILS: &str = "This message was a bounce but we couldn't link it with any outgoing message and there was no route for it.";

/// Borrowed view of either side of Postal's `MessageBounced` payload. Keeping
/// the JSON contract here prevents the incoming and original shapes drifting.
struct BounceWebhookFields<'a> {
    id: i64,
    token: &'a str,
    direction: &'a str,
    message_id: Option<String>,
    to: &'a str,
    from_address: &'a str,
    subject: String,
    timestamp: f64,
    spam_status: &'a str,
    tag: Option<&'a str>,
}

impl<'a> From<&'a StoredMessage> for BounceWebhookFields<'a> {
    fn from(message: &'a StoredMessage) -> Self {
        Self {
            id: message.id,
            token: &message.token,
            direction: &message.scope,
            message_id: postal_webhook_message_id(message.message_id_header.as_deref()),
            to: &message.rcpt_to,
            from_address: &message.mail_from,
            subject: postal_webhook_subject(message.subject.as_deref()),
            timestamp: postal_webhook_timestamp(message.created_at),
            spam_status: &message.spam_status,
            tag: message.tag.as_deref(),
        }
    }
}

impl<'a> From<&'a BounceOriginal> for BounceWebhookFields<'a> {
    fn from(message: &'a BounceOriginal) -> Self {
        Self {
            id: message.id,
            token: &message.token,
            direction: &message.scope,
            message_id: postal_webhook_message_id(message.message_id_header.as_deref()),
            to: &message.rcpt_to,
            from_address: &message.mail_from,
            subject: postal_webhook_subject(message.subject.as_deref()),
            timestamp: postal_webhook_timestamp(message.created_at),
            spam_status: &message.spam_status,
            tag: message.tag.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// Outgoing message delivered via SMTP.
    Delivered { response: String },
    /// Soft failure — requeued with backoff.
    Delayed { response: String },
    /// Terminal failure (hard fail or attempts exhausted).
    Failed { response: String },
    /// Recipient is on the tenant's suppression list.
    Held,
    /// Incoming message POSTed to its route endpoint.
    Routed,
    /// Incoming message parsed and stored as a DMARC aggregate report
    /// (route target `internal://dmarc-reports`).
    DmarcReportIngested,
    /// Incoming message recognised as an ARF spam-complaint (feedback loop)
    /// report and turned into a stream-scoped complaint for the recipient.
    FeedbackReportIngested,
    /// An inbound DSN was linked to its original outgoing message.
    BounceCorrelated,
    /// A terminal DSN already bounced this outgoing message. Its stale queue
    /// row was removed without another send or delivery event.
    AlreadyBounced,
    /// Nothing to deliver (an incoming message without an endpoint).
    NothingToDo,
    /// The queued message no longer exists.
    MessageMissing,
    /// The message's IP pool has no address on this host
    /// (`worker.host_aware_pools`). It went back to the queue without an
    /// attempt, for a host that holds one.
    ForeignPool,
    /// The message's IP pool could not be read (`worker.host_aware_pools`).
    /// It went back to the queue without an attempt and is retried after
    /// `worker.foreign_pool_delay_seconds`.
    PoolUnavailable,
    /// The worker no longer held the queue row when it came to record the
    /// result: its lease ran out and another worker took the row over, or
    /// the row is gone. Nothing was recorded; the row belongs to its new
    /// owner.
    NotOwner,
    /// Warm-up: no address of the message's pool has room left today for
    /// its receiving provider. It went back to the queue without an attempt
    /// and is looked at again after `warmup.defer_seconds`.
    WarmupDeferred,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookOutcome {
    Delivered,
    Retrying,
    GivenUp,
}

pub struct Worker {
    store: PgStore,
    sink: PgMessageSink,
    queue: PgQueue,
    webhook_queue: PgWebhookQueue,
    /// The installation lifecycle-event outbox.
    lifecycle_queue: PgLifecycleQueue,
    /// `lifecycle_webhooks`: where lifecycle events go and the HMAC secret.
    lifecycle_webhooks: camelmailer_config::LifecycleWebhooks,
    sender: SmtpSender,
    signer: Option<Signer>,
    dkim_selector: String,
    rspamd: Option<RspamdInspector>,
    clamav: Option<ClamavInspector>,
    spam_threshold: f64,
    spam_failure_threshold: f64,
    /// Base URL for tracking links, e.g. `https://track.example.com`.
    tracking_base_url: String,
    /// `camelmailer.web_protocol` — scheme for per-server track domain URLs.
    web_protocol: String,
    http: reqwest::Client,
    /// Address guard for outbound webhook / route-endpoint requests (SSRF).
    ssrf_guard: SsrfGuard,
    max_attempts: i32,
    /// Installation retention defaults (`retention` group, with
    /// `camelmailer.message_retention_days` as the metadata alias).
    retention: camelmailer_core::RetentionPeriods,
    /// `retention.auth_event_days` (0 = keep forever).
    auth_event_days: u32,
    /// `retention.api_request_days` (0 = keep forever).
    api_request_days: u32,
    /// `retention.batch_size`.
    retention_batch: i64,
    /// Validated shared SMTP return-path domain. Invalid placeholders retain
    /// the submitted envelope sender.
    return_path_domain: Option<String>,
    worker_id: String,
    /// Delivery loops of this process (`worker.threads`).
    threads: u32,
    /// `smtp_client.helo_from_ip_address`.
    helo_from_ip_address: bool,
    /// Host-aware source selection; `None` uses each pool's first address.
    source_selection: Option<SourceSelection>,
    /// Rows this process is working on, kept fresh by the heartbeat.
    in_flight: InFlight,
    /// How often the heartbeat refreshes the in-flight locks.
    heartbeat_interval: Duration,
    /// `worker.shutdown_grace_seconds`.
    shutdown_grace: Duration,
    /// Classifies recipient domains by mailbox provider for the counters.
    providers: crate::provider::ProviderClassifier,
    /// Adaptive warm-up (inactive until an address warms up).
    warmup: crate::warmup::WarmupGate,
    /// `deliverability_feedback.fbl.suppress_complainants`.
    fbl_suppress_complainants: bool,
}

impl Worker {
    pub fn new(config: &camelmailer_config::Config, store: PgStore) -> Self {
        let lease_seconds = i64::from(config.camelmailer.queue_lock_lease_seconds());
        let queue = PgQueue::with_stale_lock_seconds(store.pool().clone(), lease_seconds);
        let sink = PgMessageSink::new(store.clone());
        let sender = SmtpSender::new(config);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        let signer =
            Signer::from_pem_file(&config.camelmailer.signing_key_path).unwrap_or_else(|error| {
                tracing::warn!(%error, "could not load signing key; webhook signing disabled");
                None
            });
        let webhook_queue =
            PgWebhookQueue::with_stale_lock_seconds(store.pool().clone(), lease_seconds);
        let rspamd = config
            .rspamd
            .enabled
            .then(|| RspamdInspector::new(&config.rspamd));
        let clamav = config
            .clamav
            .enabled
            .then(|| ClamavInspector::new(&config.clamav));
        let return_path_domain = config
            .dns
            .return_path_envelope
            .then(|| config.dns.normalized_return_path_domain())
            .flatten();
        if config.dns.return_path_envelope && return_path_domain.is_none() {
            tracing::warn!(
                configured = %config.dns.return_path_domain,
                "dns.return_path_domain is empty, invalid, or reserved; preserving submitted envelope senders"
            );
        }
        let lifecycle_queue =
            PgLifecycleQueue::with_stale_lock_seconds(store.pool().clone(), lease_seconds);
        Self {
            store,
            sink,
            queue,
            webhook_queue,
            lifecycle_queue,
            lifecycle_webhooks: config.lifecycle_webhooks.clone(),
            signer,
            dkim_selector: config.dns.dkim_identifier.clone(),
            rspamd,
            clamav,
            spam_threshold: config.camelmailer.default_spam_threshold as f64,
            spam_failure_threshold: config.camelmailer.default_spam_failure_threshold as f64,
            tracking_base_url: format!(
                "{}://{}",
                config.camelmailer.web_protocol, config.dns.track_domain
            ),
            web_protocol: config.camelmailer.web_protocol.clone(),
            sender,
            http,
            ssrf_guard: SsrfGuard::from_config(config),
            max_attempts: config.camelmailer.default_maximum_delivery_attempts as i32,
            retention: camelmailer_core::RetentionPeriods::from_days(
                config.retention.message_content_days,
                config.message_metadata_retention_days(),
                config.retention.webhook_log_days,
                config.retention.dmarc_report_days,
            ),
            auth_event_days: config.retention.auth_event_days,
            api_request_days: config.retention.api_request_days,
            retention_batch: config.retention.batch_size.max(1) as i64,
            return_path_domain,
            worker_id: format!("worker-{}", camelmailer_core::token::generate_token(6)),
            threads: config.worker.threads.max(1),
            helo_from_ip_address: config.smtp_client.helo_from_ip_address,
            source_selection: config
                .worker
                .host_aware_pools
                .then(|| SourceSelection::new(&config.worker)),
            in_flight: InFlight::default(),
            heartbeat_interval: HEARTBEAT_INTERVAL,
            shutdown_grace: Duration::from_secs(u64::from(config.worker.shutdown_grace_seconds)),
            providers: crate::provider::ProviderClassifier::default(),
            warmup: crate::warmup::WarmupGate::new(&config.warmup),
            fbl_suppress_complainants: config.deliverability_feedback.fbl.suppress_complainants,
        }
    }

    /// The id this process locks rows with (`locked_by`).
    pub fn worker_id(&self) -> &str {
        &self.worker_id
    }

    /// Classify recipient domains with `providers` (tests use a fixed MX
    /// lookup).
    pub fn with_provider_classifier(
        mut self,
        providers: crate::provider::ProviderClassifier,
    ) -> Self {
        self.providers = providers;
        self
    }

    /// Override how often the heartbeat runs (tests; the default is
    /// [`HEARTBEAT_INTERVAL`]).
    pub fn with_heartbeat_interval(mut self, interval: Duration) -> Self {
        self.heartbeat_interval = interval;
        self
    }

    /// Override `worker.shutdown_grace_seconds`.
    pub fn with_shutdown_grace(mut self, grace: Duration) -> Self {
        self.shutdown_grace = grace;
        self
    }

    /// Refresh the locks of every row this process is working on, so no
    /// other worker takes them over mid-delivery. Runs every
    /// [`HEARTBEAT_INTERVAL`] from [`Worker::run`]. Returns how many rows it
    /// refreshed, and warns about any in-flight row whose lock another
    /// worker has taken over (its lease ran out while this worker could not
    /// reach the database), because that row may now be delivered twice.
    pub async fn heartbeat(&self) -> Result<u64, sqlx::Error> {
        let messages = self.in_flight.messages.ids();
        let refreshed = self.queue.heartbeat(&self.worker_id, &messages).await?;
        let mut count = self.warn_lost(
            "queued_messages",
            &self.in_flight.messages,
            &messages,
            &refreshed,
        );
        let webhooks = self.in_flight.webhooks.ids();
        let refreshed = self
            .webhook_queue
            .heartbeat(&self.worker_id, &webhooks)
            .await?;
        count += self.warn_lost(
            "webhook_requests",
            &self.in_flight.webhooks,
            &webhooks,
            &refreshed,
        );
        let lifecycle = self.in_flight.lifecycle.ids();
        let refreshed = self
            .lifecycle_queue
            .heartbeat(&self.worker_id, &lifecycle)
            .await?;
        count += self.warn_lost(
            "lifecycle_events",
            &self.in_flight.lifecycle,
            &lifecycle,
            &refreshed,
        );
        Ok(count)
    }

    /// The outcome of a delivery whose result was handed to
    /// `record_outbound_delivery`: `outcome` when it was recorded.
    fn recorded_outcome(
        &self,
        recorded: OutboundRecord,
        message_id: i64,
        outcome: ProcessOutcome,
    ) -> ProcessOutcome {
        match recorded {
            OutboundRecord::Recorded(_) => outcome,
            OutboundRecord::AlreadyBounced => ProcessOutcome::AlreadyBounced,
            OutboundRecord::NotOwner => {
                tracing::warn!(
                    message_id,
                    worker_id = %self.worker_id,
                    ?outcome,
                    "another worker took over this message's queue row before its result was recorded; the result was not recorded"
                );
                ProcessOutcome::NotOwner
            }
        }
    }

    /// Count one outbound SMTP result against the address it was sent from
    /// and the receiving provider (`ip_delivery_stats`), once the result is
    /// recorded. Counting never changes the delivery: a failure is logged
    /// and ignored.
    async fn count_delivery(
        &self,
        recorded: OutboundRecord,
        source: Option<&camelmailer_core::IpAddress>,
        domain: &str,
        message: &StoredMessage,
        kind: DeliveryKind,
        response: &str,
    ) {
        if !matches!(recorded, OutboundRecord::Recorded(_)) {
            return;
        }
        let provider = self.providers.classify(domain).await;
        let record = IpDeliveryRecord {
            day: chrono::Utc::now().date_naive(),
            source_ip: source
                .map(|address| address.ipv4.clone())
                .unwrap_or_else(|| camelmailer_core::DEFAULT_SOURCE.to_string()),
            ip_address_id: source.map(|address| address.id),
            provider,
            kind,
            policy_block: kind != DeliveryKind::Sent && camelmailer_core::is_policy_block(response),
            server_id: message.server_id,
            message_id: message.id,
        };
        if let Err(error) = DeliveryStatsStore::record_ip_delivery(&self.store, &record).await {
            tracing::warn!(%error, message_id = message.id, "could not count the delivery");
            return;
        }
        // Warm-up: a run of policy refusals today brakes the address for
        // this provider at once.
        if let (true, Some(address)) = (record.policy_block, source) {
            if self.warmup.profile(address.id).is_some() {
                self.brake_if_blocked(address, provider, record.day).await;
            }
        }
    }

    async fn brake_if_blocked(
        &self,
        address: &camelmailer_core::IpAddress,
        provider: camelmailer_core::ProviderGroup,
        day: chrono::NaiveDate,
    ) {
        let today = match DeliveryStatsStore::ip_delivery_stats(&self.store, day).await {
            Ok(stats) => stats
                .into_iter()
                .find(|s| s.day == day && s.provider == provider && s.source_ip == address.ipv4),
            Err(error) => {
                tracing::warn!(%error, "could not read today's counters for the warm-up brake");
                return;
            }
        };
        let Some(today) = today else { return };
        if let Err(error) = self
            .warmup
            .brake_if_blocked(
                &self.store,
                address.id,
                provider,
                &today,
                chrono::Utc::now(),
            )
            .await
        {
            tracing::warn!(%error, "could not apply the warm-up brake");
        }
    }

    /// Warm-up: judge yesterday for every warming address (once; several
    /// workers may try, the first to save wins). Runs from housekeeping.
    pub async fn evaluate_warmup(
        &self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<usize, camelmailer_core::StoreError> {
        self.warmup.reload(&self.store).await?;
        let day = now.date_naive() - chrono::Duration::days(1);
        let stats = DeliveryStatsStore::ip_delivery_stats(&self.store, day).await?;
        let addresses: HashMap<camelmailer_core::Id, String> =
            camelmailer_core::WarmupStore::warmup_addresses(&self.store)
                .await?
                .into_iter()
                .map(|a| (a.ip_address_id, a.ipv4))
                .collect();
        let no_signals = |_: camelmailer_core::Id, _: camelmailer_core::ProviderGroup| Vec::new();
        let written = self
            .warmup
            .evaluate_day(&self.store, day, &stats, &addresses, &no_signals, now)
            .await?;
        Ok(written.len())
    }

    /// Remove a queue row this worker holds; warns when another worker took
    /// it over meanwhile and leaves it to that worker.
    async fn complete_row(&self, id: i64) -> Result<(), sqlx::Error> {
        owned(
            "queued_messages",
            id,
            self.queue.complete(id, &self.worker_id).await?,
        );
        Ok(())
    }

    /// Reschedule a queue row this worker holds, as [`Worker::complete_row`].
    async fn retry_row(&self, id: i64, attempts: i32) -> Result<(), sqlx::Error> {
        owned(
            "queued_messages",
            id,
            self.queue.retry(id, attempts, &self.worker_id).await?,
        );
        Ok(())
    }

    /// Log the in-flight rows of `tracked` the heartbeat could not refresh
    /// although they are still in flight; returns how many it refreshed.
    fn warn_lost(&self, table: &str, tracked: &Tracked, asked: &[i64], refreshed: &[i64]) -> u64 {
        let lost: Vec<i64> = asked
            .iter()
            .copied()
            .filter(|id| !refreshed.contains(id) && tracked.contains(*id))
            .collect();
        if !lost.is_empty() {
            tracing::warn!(
                table,
                ?lost,
                "another worker took over rows this worker is still processing; they may be delivered twice"
            );
        }
        refreshed.len() as u64
    }

    /// Hand the in-flight rows that have not reached a receiver yet back to
    /// the queue, without counting an attempt, so another worker takes them
    /// at once. Called on shutdown, just before the work still running is
    /// stopped. Every other row this process holds keeps its lock until the
    /// lease runs out, as it would after a crash: a delivery stopped while
    /// talking to the receiver may already have been accepted, and a row
    /// whose processing failed half way (say after the receiver's `250`)
    /// must not be sent again straight away.
    pub async fn release_unstarted(&self) -> Result<Released, sqlx::Error> {
        let messages = self.in_flight.messages.take_unstarted();
        let webhooks = self.in_flight.webhooks.take_unstarted();
        let lifecycle = self.in_flight.lifecycle.take_unstarted();
        Ok(Released {
            messages: self.queue.release(&self.worker_id, &messages).await?,
            webhooks: self
                .webhook_queue
                .release(&self.worker_id, &webhooks)
                .await?,
            lifecycle_events: self
                .lifecycle_queue
                .release(&self.worker_id, &lifecycle)
                .await?,
        })
    }

    /// Put back the rows whose processing panicked: one attempt more and a
    /// wait of at least the lease, so a row that crashes the code cannot
    /// cycle through the workers. Rows it cannot put back keep their lock
    /// until the lease runs out.
    async fn defer_panicked(&self) {
        let messages = self.in_flight.messages.take_panicked();
        let webhooks = self.in_flight.webhooks.take_panicked();
        let lifecycle = self.in_flight.lifecycle.take_panicked();
        let results = [
            (
                "queued_messages",
                &messages,
                self.queue.defer_panicked(&self.worker_id, &messages).await,
            ),
            (
                "webhook_requests",
                &webhooks,
                self.webhook_queue
                    .defer_panicked(&self.worker_id, &webhooks)
                    .await,
            ),
            (
                "lifecycle_events",
                &lifecycle,
                self.lifecycle_queue
                    .defer_panicked(&self.worker_id, &lifecycle)
                    .await,
            ),
        ];
        for (table, ids, result) in results {
            match result {
                Ok(_) if ids.is_empty() => {}
                Ok(deferred) => tracing::warn!(
                    table,
                    ?ids,
                    deferred,
                    "deferred rows whose processing panicked"
                ),
                Err(error) => {
                    tracing::error!(table, ?ids, %error, "could not defer rows whose processing panicked; they return after the lock lease")
                }
            }
        }
    }

    /// Apply `worker.host_aware_pools`, `worker.source_addresses` and
    /// `worker.foreign_pool_delay_seconds` from `settings` (the worker
    /// settings with their environment overrides).
    pub fn with_source_selection(mut self, settings: &camelmailer_config::Worker) -> Self {
        self.source_selection = settings
            .host_aware_pools
            .then(|| SourceSelection::new(settings));
        if self.source_selection.is_none() {
            self.queue.set_source_pools(None);
        }
        self
    }

    /// Host-aware workers: tell the queue which pools this host holds an
    /// address of, so it skips the others' mail. At most every 30 s.
    async fn refresh_source_pools(&self) -> Result<(), sqlx::Error> {
        let Some(selection) = &self.source_selection else {
            return Ok(());
        };
        {
            let mut refreshed = selection.pools_refreshed.lock().expect("refresh lock");
            if refreshed.is_some_and(|at| at.elapsed() < LOCAL_ADDRESS_TTL) {
                return Ok(());
            }
            *refreshed = Some(Instant::now());
        }
        // Through the storage traits: every pool, then its addresses. Pools
        // are few; this runs at most every 30 s per process.
        let store_error = |e: camelmailer_core::StoreError| sqlx::Error::Protocol(e.to_string());
        let is_local = selection.local_check();
        let mut pools: Vec<i64> = Vec::new();
        for pool in camelmailer_core::AdminStore::list_ip_pools(&self.store)
            .await
            .map_err(store_error)?
        {
            let addresses = camelmailer_core::AdminStore::list_ip_addresses(&self.store, pool.id)
                .await
                .map_err(store_error)?;
            if addresses.iter().any(|address| is_local(&address.ipv4)) {
                pools.push(pool.id as i64);
            }
        }
        pools.sort_unstable();
        selection.clear_reports(&pools);
        self.queue.set_source_pools(Some(pools));
        Ok(())
    }

    /// Scope the delivery to part of the queue and set how many messages
    /// this process delivers at once. The filter comes from `worker.servers`,
    /// `worker.exclude_servers` and `worker.deliver`, resolved to server ids.
    pub fn with_delivery(mut self, filter: QueueFilter, threads: u32) -> Self {
        self.queue = self.queue.with_filter(filter);
        self.threads = threads.max(1);
        self
    }

    /// Process one queued message, if any is ready. Returns `None` when the
    /// queue is empty.
    pub async fn process_next(&self) -> Result<Option<ProcessOutcome>, sqlx::Error> {
        if let Err(error) = self.refresh_source_pools().await {
            tracing::warn!(%error, "could not refresh the local IP pools");
        }
        if let Err(error) = self.warmup.refresh(&self.store).await {
            tracing::warn!(%error, "could not read the warm-up settings");
        }
        let Some(queued) = self.queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };
        let _held = self.in_flight.messages.hold(queued.id);

        let loaded = if self.return_path_domain.is_some() {
            self.sink
                .message_with_server_token(queued.server_id, queued.message_id)
                .await?
                .map(|(message, token)| (message, Some(token)))
        } else {
            self.sink
                .message_by_id(queued.server_id, queued.message_id)
                .await?
                .map(|message| (message, None))
        };
        let Some((mut message, server_token)) = loaded else {
            self.complete_row(queued.id).await?;
            return Ok(Some(ProcessOutcome::MessageMissing));
        };

        let outcome = if message.scope == "outgoing" {
            self.process_outgoing(&queued, &message, server_token.as_deref())
                .await?
        } else {
            // Inbound processing forwards to routes (HTTP endpoints, SMTP),
            // so the whole of it counts as talking to a receiver.
            self.in_flight.messages.sending(queued.id);
            self.process_incoming(&queued, &mut message).await?
        };
        Ok(Some(outcome))
    }

    async fn process_outgoing(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
        server_token: Option<&str>,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        if message.status == "Bounced" {
            self.complete_row(queued.id).await?;
            tracing::info!(
                message_id = message.id,
                "completed stale queue row for already bounced message"
            );
            return Ok(ProcessOutcome::AlreadyBounced);
        }

        // Suppression list check (tenant-scoped, RLS-protected). Stream-aware:
        // a server-wide (stream_id NULL) or this-stream suppression blocks it,
        // so a broadcast opt-out never holds transactional mail.
        if camelmailer_core::ServerStore::address_suppressed(
            &self.store,
            message.server_id,
            &message.rcpt_to,
            message.stream_id,
        )
        .await
        .map_err(|e| sqlx::Error::Protocol(e.to_string()))?
        {
            let details = "recipient is on the suppression list";
            let payload = self.message_payload(message, details);
            let recorded = self
                .sink
                .record_outbound_delivery(
                    message.server_id,
                    message.id,
                    queued.id,
                    &self.worker_id,
                    "Held",
                    details,
                    "",
                    false,
                    None,
                    OutboundQueueAction::Complete,
                    "MessageHeld",
                    &payload,
                )
                .await?;
            return Ok(self.recorded_outcome(recorded, message.id, ProcessOutcome::Held));
        }

        // Rewrite HTML links for click tracking and append an open pixel
        // before signing, so the DKIM signature covers the final body.
        let tracked = self.apply_tracking(message).await?;

        // DKIM-sign at delivery time when the message carries an
        // authenticated domain: with the domain's own key when it has one,
        // with the installation key otherwise. The stored message stays
        // unsigned, matching the Ruby behaviour.
        let identified =
            camelmailer_core::bounce::add_message_token_header(&tracked, &message.token);
        let raw_message = match message.domain_id {
            Some(domain_id) => match self.store.domain_by_id(domain_id).await {
                Ok(Some(domain)) => match dkim::signer_for_domain(
                    domain.dkim_private_key.as_deref(),
                    self.signer.as_ref(),
                ) {
                    Some(signer) => dkim::sign_and_prepend(
                        &identified,
                        &domain.name,
                        &self.dkim_selector,
                        &signer,
                        chrono::Utc::now().timestamp(),
                    ),
                    None => identified,
                },
                _ => identified,
            },
            None => identified,
        };

        let return_path = if let Some(domain) = &self.return_path_domain {
            let token = server_token.ok_or(sqlx::Error::RowNotFound)?;
            format!("{token}@{domain}")
        } else {
            message.mail_from.clone()
        };

        // Source-address selection: send from the message stream's IP pool if
        // it sets one, else the server's pool. A stream without a pool
        // resolves to the server pool exactly as before.
        // Every address the delivery may go out from, for warm-up overflow.
        let mut candidates: Vec<camelmailer_core::IpAddress> = Vec::new();
        let source: Option<camelmailer_core::IpAddress> =
            if let Some(selection) = &self.source_selection {
                // Host-aware: only the pool addresses this host holds. A pool
                // with none here goes back to the queue for a host that has one;
                // no pool at all keeps the OS default address.
                let addresses = match camelmailer_core::ServerStore::source_addresses_for(
                    &self.store,
                    message.server_id,
                    message.stream_id,
                )
                .await
                {
                    Ok(addresses) => addresses,
                    Err(error) => {
                        // The row is locked: hand it back without an attempt
                        // instead of leaving it locked until the stale-lock
                        // reclaim. Sending from an address the pool may not
                        // hold would be worse than a short wait.
                        tracing::warn!(
                            message_id = message.id,
                            %error,
                            "could not read the IP pool; handing the message back"
                        );
                        self.queue
                            .defer(
                                queued.id,
                                &self.worker_id,
                                selection.foreign_pool_delay_seconds,
                                None,
                            )
                            .await?;
                        return Ok(ProcessOutcome::PoolUnavailable);
                    }
                };
                if addresses.is_empty() {
                    None
                } else {
                    let is_local = selection.local_check();
                    let local: Vec<&camelmailer_core::IpAddress> =
                        addresses.iter().filter(|a| is_local(&a.ipv4)).collect();
                    candidates = local.iter().map(|a| (*a).clone()).collect();
                    match choose_source_address(&local, &queued.domain) {
                        Some(address) => Some(address.clone()),
                        None => {
                            let pool_id = addresses[0].ip_pool_id;
                            selection.report_foreign(pool_id);
                            // Record the pool as resolved now, so the queue
                            // filter of the hosts without an address of it skips
                            // the row from here on (rows queued before the
                            // column, or after the pool changed).
                            self.queue
                                .defer(
                                    queued.id,
                                    &self.worker_id,
                                    selection.foreign_pool_delay_seconds,
                                    Some(pool_id as i64),
                                )
                                .await?;
                            return Ok(ProcessOutcome::ForeignPool);
                        }
                    }
                }
            } else {
                // The pool's first address (lowest priority number, then id).
                let addresses = camelmailer_core::ServerStore::source_addresses_for(
                    &self.store,
                    message.server_id,
                    message.stream_id,
                )
                .await
                .ok()
                .unwrap_or_default();
                candidates = addresses.clone();
                addresses.into_iter().next()
            };
        // Warm-up: only when an address of the pool warms up. With warm-up
        // off everywhere this is skipped and the source stays as chosen.
        let source = match source {
            Some(chosen) if self.warmup.covers(&candidates) => {
                let provider = self.providers.classify(&queued.domain).await;
                match self
                    .warmup
                    .pick(
                        &self.store,
                        &chosen,
                        &candidates,
                        provider,
                        chrono::Utc::now(),
                    )
                    .await
                {
                    crate::warmup::Pick::Use(address) => Some(address),
                    crate::warmup::Pick::Wait => {
                        tracing::info!(
                            message_id = message.id,
                            provider = provider.as_str(),
                            "no warm-up allowance left in the pool today; the message waits"
                        );
                        owned(
                            "queued_messages",
                            queued.id,
                            self.queue
                                .defer(queued.id, &self.worker_id, self.warmup.defer_seconds, None)
                                .await?,
                        );
                        return Ok(ProcessOutcome::WarmupDeferred);
                    }
                }
            }
            other => other,
        };
        let source_ip: Option<IpAddr> = source.as_ref().and_then(|a| a.ipv4.parse().ok());
        // EHLO with the bound address's own name when asked to and it has
        // one; otherwise the installation's name.
        let helo_hostname = source
            .as_ref()
            .filter(|_| self.helo_from_ip_address && source_ip.is_some())
            .and_then(pool_helo_hostname);

        // The address families to use and the local addresses to bind. Looked
        // up before the row is marked as sending, so a shutdown during the
        // lookup still hands the row back at once.
        let outbound = crate::outbound::source_for(
            &self.store,
            &self.sender,
            message.server_id,
            message.stream_id,
            source_ip,
        )
        .await;

        // Until the client sends DATA a shutdown may hand the row back (the
        // gate); from DATA on the row keeps its lock for the lease.
        let data_gate = self.in_flight.messages.delivering(queued.id);
        let outcome = self
            .sender
            .send_gated(
                &queued.domain,
                &return_path,
                &message.rcpt_to,
                &raw_message,
                outbound,
                helo_hostname,
                data_gate,
            )
            .await;

        match outcome {
            SendOutcome::Sent { response, tls } => {
                let payload = self.message_payload(message, &response);
                let recorded = self
                    .sink
                    .record_outbound_delivery(
                        message.server_id,
                        message.id,
                        queued.id,
                        &self.worker_id,
                        "Sent",
                        "message accepted by the remote server",
                        &response,
                        tls,
                        None,
                        OutboundQueueAction::Complete,
                        "MessageSent",
                        &payload,
                    )
                    .await?;
                if recorded == OutboundRecord::AlreadyBounced {
                    tracing::info!(
                        message_id = message.id,
                        "preserved newer bounced state after outbound SMTP success"
                    );
                }
                self.count_delivery(
                    recorded,
                    source.as_ref(),
                    &queued.domain,
                    message,
                    DeliveryKind::Sent,
                    &response,
                )
                .await;
                Ok(self.recorded_outcome(
                    recorded,
                    message.id,
                    ProcessOutcome::Delivered { response },
                ))
            }
            SendOutcome::SoftFail { response } => {
                if queued.attempts + 1 >= self.max_attempts {
                    // Terminal failure: classify the bounce from the last
                    // SMTP response (5xx -> hard, 4xx -> soft, otherwise
                    // undetermined — see camelmailer_core::bounce).
                    let category = camelmailer_core::bounce::classify_response(&response);
                    let payload = self.message_payload(message, &response);
                    let recorded = self
                        .sink
                        .record_outbound_delivery(
                            message.server_id,
                            message.id,
                            queued.id,
                            &self.worker_id,
                            "HardFail",
                            "delivery attempts exhausted",
                            &response,
                            false,
                            Some(category.as_str()),
                            OutboundQueueAction::Complete,
                            "MessageDeliveryFailed",
                            &payload,
                        )
                        .await?;
                    self.count_delivery(
                        recorded,
                        source.as_ref(),
                        &queued.domain,
                        message,
                        DeliveryKind::Deferred,
                        &response,
                    )
                    .await;
                    Ok(self.recorded_outcome(
                        recorded,
                        message.id,
                        ProcessOutcome::Failed { response },
                    ))
                } else {
                    let payload = self.message_payload(message, &response);
                    let recorded = self
                        .sink
                        .record_outbound_delivery(
                            message.server_id,
                            message.id,
                            queued.id,
                            &self.worker_id,
                            "SoftFail",
                            "temporary delivery failure",
                            &response,
                            false,
                            // transient — the message may still deliver, so
                            // no bounce category is persisted yet
                            None,
                            OutboundQueueAction::Retry {
                                attempts: queued.attempts,
                            },
                            "MessageDelayed",
                            &payload,
                        )
                        .await?;
                    self.count_delivery(
                        recorded,
                        source.as_ref(),
                        &queued.domain,
                        message,
                        DeliveryKind::Deferred,
                        &response,
                    )
                    .await;
                    Ok(self.recorded_outcome(
                        recorded,
                        message.id,
                        ProcessOutcome::Delayed { response },
                    ))
                }
            }
            SendOutcome::HardFail { response } => {
                let category = camelmailer_core::bounce::classify_response(&response);
                let payload = self.message_payload(message, &response);
                let recorded = self
                    .sink
                    .record_outbound_delivery(
                        message.server_id,
                        message.id,
                        queued.id,
                        &self.worker_id,
                        "HardFail",
                        "message rejected by the remote server",
                        &response,
                        false,
                        Some(category.as_str()),
                        OutboundQueueAction::Complete,
                        "MessageDeliveryFailed",
                        &payload,
                    )
                    .await?;
                self.count_delivery(
                    recorded,
                    source.as_ref(),
                    &queued.domain,
                    message,
                    DeliveryKind::Bounced,
                    &response,
                )
                .await;
                Ok(
                    self.recorded_outcome(
                        recorded,
                        message.id,
                        ProcessOutcome::Failed { response },
                    ),
                )
            }
        }
    }

    /// Inspect an incoming message with rspamd/clamav (when enabled) and
    /// record the verdict. Returns true when the message is a virus threat
    /// or exceeds the spam-failure threshold and should be held.
    async fn inspect(&self, message: &mut StoredMessage) -> Result<bool, sqlx::Error> {
        if self.rspamd.is_none() && self.clamav.is_none() {
            return Ok(false);
        }
        if message.inspected {
            return Ok(message.threat || message.spam_status == "SpamFailure");
        }

        let mut spam_status = "NotChecked".to_string();
        let mut spam_score = 0.0;
        if let Some(rspamd) = &self.rspamd {
            match rspamd
                .check(&message.raw_message, self.spam_threshold)
                .await
            {
                Ok(result) => {
                    spam_score = result.score;
                    spam_status = if result.score >= self.spam_failure_threshold {
                        "SpamFailure".to_string()
                    } else if result.score >= self.spam_threshold {
                        "Spam".to_string()
                    } else {
                        "NotSpam".to_string()
                    };
                }
                Err(error) => tracing::warn!(%error, "rspamd inspection failed"),
            }
        }

        let mut threat = false;
        let mut threat_details = None;
        if let Some(clamav) = &self.clamav {
            match clamav.scan(&message.raw_message).await {
                Ok(result) => {
                    threat = result.found;
                    threat_details = result.details;
                }
                Err(error) => tracing::warn!(%error, "clamav inspection failed"),
            }
        }

        self.sink
            .record_inspection(
                message.server_id,
                message.id,
                &spam_status,
                spam_score,
                threat,
                threat_details.as_deref(),
            )
            .await?;

        // Later inbound handlers build payloads from this value. Keep it in
        // sync with the verdict just persisted instead of using the row that
        // was loaded before inspection.
        message.spam_status = spam_status.clone();
        message.spam_score = spam_score;
        message.threat = threat;
        message.threat_details = threat_details;
        message.inspected = true;

        Ok(threat || spam_status == "SpamFailure")
    }

    async fn process_incoming(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &mut StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        // Inspect every incoming message before any content-specific handler;
        // a virus or spam-failure message is held (stored, not delivered).
        if self.inspect(message).await? {
            self.sink
                .record_delivery(
                    message.server_id,
                    message.id,
                    "Held",
                    "message failed inspection (spam or virus)",
                    "",
                    false,
                    None,
                )
                .await?;
            self.complete_row(queued.id).await?;
            return Ok(ProcessOutcome::Held);
        }

        // Feedback-loop (ARF) reports: an ISP delivers a spam complaint as a
        // multipart/report; report-type=feedback-report. Recognise it by its
        // envelope (content-based, independent of routing) and record a
        // stream-scoped complaint for the recipient who complained.
        if crate::arf::is_feedback_report(&message.raw_message) {
            return self.ingest_feedback_report(queued, message).await;
        }

        // Correlate only machine-readable delivery-status notifications.
        // Auto-replies and other messages addressed to the return path retain
        // the existing route/fallback behaviour below.
        if let Some(outcome) = self.correlate_bounce(queued, message).await? {
            return Ok(outcome);
        }

        let route = match message.route_id {
            Some(route_id) => self
                .store
                .route_by_id(message.server_id, route_id)
                .await
                .unwrap_or(None),
            None => None,
        };

        let endpoint_url = route.as_ref().and_then(|r| {
            (r.mode == RouteMode::Endpoint)
                .then(|| r.endpoint_url.clone())
                .flatten()
        });

        let Some(endpoint_url) = endpoint_url else {
            // Accept/Hold routes and bounces: the message is stored; there is
            // nothing to deliver.
            self.complete_row(queued.id).await?;
            return Ok(ProcessOutcome::NothingToDo);
        };

        // The internal DMARC target: parse the message as an aggregate
        // report instead of POSTing it anywhere.
        if endpoint_url == camelmailer_core::DMARC_REPORTS_ENDPOINT {
            return self.ingest_dmarc_report(queued, message).await;
        }

        // SSRF guard: refuse to POST to a route endpoint that resolves to a
        // non-global (loopback / private / link-local / …) address unless the
        // operator explicitly allowed it. Treat a blocked endpoint like any
        // other failed delivery (retry with backoff, then terminal fail).
        if let Err(error) = self.ssrf_guard.check_url(&endpoint_url).await {
            tracing::warn!(%error, endpoint = %endpoint_url, "route endpoint blocked by SSRF guard");
            let response = format!("route endpoint blocked by address guard: {error}");
            return if queued.attempts + 1 >= self.max_attempts {
                self.complete_row(queued.id).await?;
                Ok(ProcessOutcome::Failed { response })
            } else {
                self.retry_row(queued.id, queued.attempts).await?;
                Ok(ProcessOutcome::Delayed { response })
            };
        }

        let payload = json!({
            "message": {
                "id": message.id,
                "token": message.token,
                "rcpt_to": message.rcpt_to,
                "mail_from": message.mail_from,
                "bounce": message.bounce,
            },
            "raw_base64": base64::engine::general_purpose::STANDARD.encode(&message.raw_message),
        });

        let result = self.http.post(&endpoint_url).json(&payload).send().await;
        let success = matches!(&result, Ok(response) if response.status().is_success());
        if success {
            self.complete_row(queued.id).await?;
            Ok(ProcessOutcome::Routed)
        } else {
            let response = match result {
                Ok(response) => format!("endpoint returned {}", response.status()),
                Err(error) => format!("endpoint request failed: {error}"),
            };
            if queued.attempts + 1 >= self.max_attempts {
                self.complete_row(queued.id).await?;
                Ok(ProcessOutcome::Failed { response })
            } else {
                self.retry_row(queued.id, queued.attempts).await?;
                Ok(ProcessOutcome::Delayed { response })
            }
        }
    }

    async fn correlate_bounce(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<Option<ProcessOutcome>, sqlx::Error> {
        if !message.bounce
            || !camelmailer_core::bounce::is_failed_delivery_status_notification(
                &message.raw_message,
            )
        {
            return Ok(None);
        }
        let category = camelmailer_core::bounce::classify_dsn(&message.raw_message);
        let tokens = camelmailer_core::bounce::original_message_tokens(&message.raw_message);
        let correlation = self
            .sink
            .correlate_bounce(
                message.server_id,
                message.id,
                &tokens,
                category.as_str(),
                |original| {
                    json!({
                        "original_message": self.bounce_webhook_payload(original.into()),
                        "bounce": self.bounce_webhook_payload(message.into()),
                    })
                },
            )
            .await?;
        match correlation {
            BounceCorrelationOutcome::Correlated(_) => {}
            BounceCorrelationOutcome::AlreadyCorrelated => {
                self.complete_row(queued.id).await?;
                return Ok(Some(ProcessOutcome::BounceCorrelated));
            }
            BounceCorrelationOutcome::NotMatched if message.route_id.is_none() => {
                self.sink
                    .record_unmatched_bounce(
                        message.server_id,
                        message.id,
                        queued.id,
                        category.as_str(),
                        UNMATCHED_BOUNCE_DETAILS,
                    )
                    .await?;
                return Ok(Some(ProcessOutcome::Failed {
                    response: UNMATCHED_BOUNCE_DETAILS.to_string(),
                }));
            }
            BounceCorrelationOutcome::NotMatched => return Ok(None),
        }
        self.complete_row(queued.id).await?;
        Ok(Some(ProcessOutcome::BounceCorrelated))
    }

    /// Parse an inbound message as a DMARC aggregate report and store it
    /// in the tenant's report tables. Parse failures hold the message
    /// (like any undeliverable inbound mail); storage failures retry with
    /// backoff. Never panics — a malformed report must not take the
    /// worker down.
    async fn ingest_dmarc_report(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        let report = match crate::dmarc::extract_report(&message.raw_message) {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!(%error, message_id = message.id, "unparseable DMARC report held");
                self.complete_row(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "Held",
                        "message could not be parsed as a DMARC aggregate report",
                        &error.to_string(),
                        false,
                        None,
                    )
                    .await?;
                return Ok(ProcessOutcome::Held);
            }
        };

        let new = camelmailer_core::NewDmarcReport {
            server_id: message.server_id,
            domain: report.domain,
            org_name: report.org_name,
            org_email: report.org_email,
            report_id: report.report_id,
            date_range_begin: report.date_range_begin,
            date_range_end: report.date_range_end,
            records: report
                .records
                .into_iter()
                .map(|record| camelmailer_core::NewDmarcRecord {
                    source_ip: record.source_ip,
                    count: record.count,
                    disposition: record.disposition,
                    dkim_result: record.dkim_result,
                    spf_result: record.spf_result,
                    dkim_aligned: record.dkim_aligned,
                    spf_aligned: record.spf_aligned,
                    header_from: record.header_from,
                    envelope_from: record.envelope_from,
                })
                .collect(),
        };
        match camelmailer_core::ServerStore::store_dmarc_report(&self.store, new).await {
            Ok(stored) => {
                self.complete_row(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "Processed",
                        &format!(
                            "DMARC aggregate report for {} stored (#{}, {} records)",
                            stored.domain, stored.id, stored.record_count
                        ),
                        "",
                        false,
                        None,
                    )
                    .await?;
                Ok(ProcessOutcome::DmarcReportIngested)
            }
            Err(error) => {
                // storage trouble is transient — retry like a failing
                // endpoint instead of losing the report
                tracing::warn!(%error, message_id = message.id, "could not store DMARC report");
                let response = format!("could not store the DMARC report: {error}");
                if queued.attempts + 1 >= self.max_attempts {
                    self.complete_row(queued.id).await?;
                    Ok(ProcessOutcome::Failed { response })
                } else {
                    self.retry_row(queued.id, queued.attempts).await?;
                    Ok(ProcessOutcome::Delayed { response })
                }
            }
        }
    }

    /// Parse an inbound message as an ARF feedback-loop (spam-complaint)
    /// report and, when it is an abuse report, record the complaint and queue
    /// `MessageComplained`. A broadcast recipient (resolved via the
    /// List-Unsubscribe token) also gets a stream-scoped complaint
    /// suppression and opt-out. Parse failures hold the message (like any
    /// undeliverable inbound mail); storage failures retry with backoff.
    /// Mirrors [`Self::ingest_dmarc_report`]; never panics.
    async fn ingest_feedback_report(
        &self,
        queued: &camelmailer_db::QueuedMessageRow,
        message: &StoredMessage,
    ) -> Result<ProcessOutcome, sqlx::Error> {
        let report = match crate::arf::extract_feedback(&message.raw_message) {
            Ok(report) => report,
            Err(error) => {
                tracing::warn!(%error, message_id = message.id, "unparseable ARF report held");
                self.complete_row(queued.id).await?;
                self.sink
                    .record_delivery(
                        message.server_id,
                        message.id,
                        "Held",
                        "message could not be parsed as an ARF feedback report",
                        &error.to_string(),
                        false,
                        None,
                    )
                    .await?;
                return Ok(ProcessOutcome::Held);
            }
        };

        // Abuse reports become complaints. A broadcast original resolves via
        // its List-Unsubscribe token (and is suppressed for that stream); any
        // other original resolves via the engine message token, Message-ID,
        // or recipient. Either way subscribed webhooks get
        // `MessageComplained`. Non-abuse feedback types are stored but
        // actioned no further, like an accepted inbound with nothing to
        // deliver.
        if report.feedback_type == "abuse" {
            let complaint = camelmailer_db::FeedbackComplaint {
                feedback_type: report.feedback_type.clone(),
                user_agent: report.user_agent.clone(),
                unsubscribe_token: report.unsubscribe_token.clone(),
                message_tokens: report.message_tokens.clone(),
                message_id: report.original_message_id.clone(),
                recipient: report.recipient(),
                suppress_transactional: self.fbl_suppress_complainants,
            };
            // Count the complaint for the provider that reported it (JMRP,
            // CFL, ...). Per-IP crediting comes from the IP metrics. A failure
            // here never holds up the complaint itself.
            if let Err(error) = camelmailer_core::FeedbackStore::record_fbl_complaint(
                &self.store,
                report.provider(),
                chrono::Utc::now().date_naive(),
            )
            .await
            {
                tracing::warn!(%error, provider = report.provider(), "could not count the feedback-loop complaint");
            }
            match self
                .sink
                .record_feedback_complaint(message.server_id, &complaint)
                .await
            {
                Ok(outcome) => {
                    self.complete_row(queued.id).await?;
                    if let FeedbackComplaintOutcome::Recorded {
                        message_id: Some(id),
                        ..
                    } = &outcome
                    {
                        // Count the complaint against the address that sent
                        // the message (best effort, like every counter).
                        if let Err(error) = DeliveryStatsStore::record_ip_complaint(
                            &self.store,
                            *id,
                            chrono::Utc::now(),
                        )
                        .await
                        {
                            tracing::warn!(%error, message_id = id, "could not count the complaint");
                        }
                    }
                    let detail = match outcome {
                        FeedbackComplaintOutcome::Recorded {
                            suppressed: true,
                            message_id,
                            ..
                        } => match message_id {
                            Some(id) if complaint.unsubscribe_token.is_none() => format!(
                                "spam complaint recorded for <msg:{id}>; recipient suppressed"
                            ),
                            _ => "spam complaint recorded for the broadcast recipient".to_string(),
                        },
                        FeedbackComplaintOutcome::Recorded {
                            message_id: Some(id),
                            ..
                        } => format!("spam complaint recorded for <msg:{id}>"),
                        FeedbackComplaintOutcome::Recorded { address, .. } => {
                            format!("spam complaint recorded for {address}; no matching message")
                        }
                        FeedbackComplaintOutcome::NotMatched => {
                            "feedback report parsed; no matching message or recipient".to_string()
                        }
                    };
                    self.sink
                        .record_delivery(
                            message.server_id,
                            message.id,
                            "Processed",
                            &detail,
                            "",
                            false,
                            None,
                        )
                        .await?;
                    Ok(ProcessOutcome::FeedbackReportIngested)
                }
                Err(error) => {
                    // storage trouble is transient — retry like a failing
                    // endpoint instead of losing the complaint
                    tracing::warn!(%error, message_id = message.id, "could not record feedback complaint");
                    let response = format!("could not record the feedback complaint: {error}");
                    if queued.attempts + 1 >= self.max_attempts {
                        self.complete_row(queued.id).await?;
                        Ok(ProcessOutcome::Failed { response })
                    } else {
                        self.retry_row(queued.id, queued.attempts).await?;
                        Ok(ProcessOutcome::Delayed { response })
                    }
                }
            }
        } else {
            // Recognised ARF of a non-abuse type. Store and move on.
            self.complete_row(queued.id).await?;
            self.sink
                .record_delivery(
                    message.server_id,
                    message.id,
                    "Processed",
                    &format!(
                        "feedback report ({}) recorded; no complaint actioned",
                        report.feedback_type
                    ),
                    "",
                    false,
                    None,
                )
                .await?;
            Ok(ProcessOutcome::FeedbackReportIngested)
        }
    }

    /// Register tracking tokens and rewrite the HTML body of an outgoing
    /// message. No-op for non-HTML messages. Returns the (possibly
    /// rewritten) raw message.
    async fn apply_tracking(&self, message: &StoredMessage) -> Result<Vec<u8>, sqlx::Error> {
        // Never rewrite a cryptographically signed message: injecting the open
        // pixel or rewriting a link would invalidate an S/MIME, PGP/MIME or
        // inline-PGP signature over the body.
        if tracking::is_signed(&message.raw_message) {
            return Ok(message.raw_message.clone());
        }

        // The decoded HTML of the first text/html part; a message without one
        // (or in a charset that cannot be rewritten safely) goes as it came.
        let Some(body) = tracking::html_of(&message.raw_message) else {
            return Ok(message.raw_message.clone());
        };

        // Per-server track domains take precedence over the installation-wide
        // `dns.track_domain`; a lookup failure falls back to the global base
        // rather than failing the delivery.
        let tracking_base_url =
            camelmailer_core::AdminStore::effective_track_domain(&self.store, message.server_id)
                .await
                .ok()
                .flatten()
                .map(|host| format!("{}://{host}", self.web_protocol))
                .unwrap_or_else(|| self.tracking_base_url.clone());

        // register a link + click token for every rewritten URL
        let mut pending_links: Vec<String> = Vec::new();
        let (rewritten, urls) = tracking::rewrite_links(&body, |url| {
            pending_links.push(url.to_string());
            format!("__CM_CLICK_{}__", pending_links.len() - 1)
        });
        let _ = urls;

        let mut click_tokens = Vec::with_capacity(pending_links.len());
        for url in &pending_links {
            let (link_id, _) = self
                .sink
                .create_link(message.server_id, message.id, url)
                .await?;
            let token = self
                .store
                .create_click_token(message.server_id, message.id, link_id, url)
                .await?;
            click_tokens.push(token);
        }

        let mut rewritten = rewritten;
        for (index, token) in click_tokens.iter().enumerate() {
            rewritten = rewritten.replace(
                &format!("__CM_CLICK_{index}__"),
                &format!("{tracking_base_url}/track/c/{token}"),
            );
        }

        // open-tracking pixel
        let open_token = self
            .store
            .create_open_token(message.server_id, message.id)
            .await?;
        let pixel_url = format!("{tracking_base_url}/track/o/{open_token}.gif");
        let rewritten = tracking::inject_open_pixel(&rewritten, &pixel_url);

        Ok(tracking::with_html(&message.raw_message, &rewritten)
            .unwrap_or_else(|| message.raw_message.clone()))
    }

    fn message_payload(&self, message: &StoredMessage, details: &str) -> serde_json::Value {
        json!({
            "message": {
                "id": message.id,
                "token": message.token,
                "rcpt_to": message.rcpt_to,
                "mail_from": message.mail_from,
                "scope": message.scope,
                "bounce": message.bounce,
            },
            "details": details,
        })
    }

    /// Postal's `MessageBounced` contract uses `direction`, `to` and `from`.
    /// Keep those names so Postal webhook consumers can migrate unchanged.
    fn bounce_webhook_payload(&self, message: BounceWebhookFields<'_>) -> serde_json::Value {
        json!({
            "id": message.id,
            "token": message.token,
            "direction": message.direction,
            "message_id": message.message_id,
            "to": message.to,
            "from": message.from_address,
            "subject": message.subject,
            "timestamp": message.timestamp,
            "spam_status": message.spam_status,
            "tag": message.tag,
        })
    }

    /// Deliver one queued webhook request, if any is ready. Signs the body
    /// with the installation signing key when the webhook asks for it,
    /// records every attempt in the tenant-scoped audit log, and retries
    /// failures with backoff.
    pub async fn process_next_webhook(&self) -> Result<Option<WebhookOutcome>, sqlx::Error> {
        let Some(request) = self.webhook_queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };
        let held = self.in_flight.webhooks.hold(request.id);

        // SSRF guard: never fetch a webhook URL that resolves to a non-global
        // address (would otherwise leak up to 2 KB of the reply into the
        // tenant-visible audit log). Record the blocked attempt and retry /
        // give up like any other failure — but never make the request.
        let attempt = request.attempts + 1;
        if let Err(error) = self.ssrf_guard.check_url(&request.url).await {
            tracing::warn!(%error, url = %request.url, "webhook blocked by SSRF guard");
            let detail = format!("blocked by address guard: {error}");
            self.webhook_queue
                .log_attempt(&request, attempt, None, false, &detail)
                .await?;
            return if attempt >= WEBHOOK_MAX_ATTEMPTS {
                owned(
                    "webhook_requests",
                    request.id,
                    self.webhook_queue
                        .complete(request.id, &self.worker_id)
                        .await?,
                );
                Ok(Some(WebhookOutcome::GivenUp))
            } else {
                owned(
                    "webhook_requests",
                    request.id,
                    self.webhook_queue
                        .retry(request.id, request.attempts, &self.worker_id)
                        .await?,
                );
                Ok(Some(WebhookOutcome::Retrying))
            };
        }

        let mut http_request = self
            .http
            .post(&request.url)
            .header("content-type", "application/json");
        // custom webhook headers first (values are secrets — never logged),
        // then the platform headers, so the latter always win
        for (name, value) in &request.headers {
            use reqwest::header::{HeaderName, HeaderValue};
            match (
                HeaderName::try_from(name.as_str()),
                HeaderValue::try_from(value.as_str()),
            ) {
                (Ok(name), Ok(value)) => {
                    http_request = http_request.header(name, value);
                }
                _ => tracing::warn!(header = %name, "skipping invalid webhook header"),
            }
        }
        // `CamelMailer` here is frozen wire vocabulary, see the note in
        // `camelmailer_api::webhook_send`.
        http_request = http_request
            .header("X-CamelMailer-Event", &request.event)
            .header("X-CamelMailer-UUID", &request.uuid);
        if request.sign {
            if let Some(signer) = &self.signer {
                let signature = signer.sign_sha256(request.payload.as_bytes());
                http_request = http_request.header(
                    "X-CamelMailer-Signature",
                    base64::engine::general_purpose::STANDARD.encode(signature),
                );
            }
        }

        held.sending();
        let result = http_request.body(request.payload.clone()).send().await;
        let (status_code, success, response_body) = match result {
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                (Some(status.as_u16() as i32), status.is_success(), body)
            }
            Err(error) => (None, false, error.to_string()),
        };
        self.webhook_queue
            .log_attempt(&request, attempt, status_code, success, &response_body)
            .await?;

        if success {
            owned(
                "webhook_requests",
                request.id,
                self.webhook_queue
                    .complete(request.id, &self.worker_id)
                    .await?,
            );
            Ok(Some(WebhookOutcome::Delivered))
        } else if attempt >= WEBHOOK_MAX_ATTEMPTS {
            owned(
                "webhook_requests",
                request.id,
                self.webhook_queue
                    .complete(request.id, &self.worker_id)
                    .await?,
            );
            tracing::warn!(url = %request.url, "webhook given up after {WEBHOOK_MAX_ATTEMPTS} attempts");
            Ok(Some(WebhookOutcome::GivenUp))
        } else {
            owned(
                "webhook_requests",
                request.id,
                self.webhook_queue
                    .retry(request.id, request.attempts, &self.worker_id)
                    .await?,
            );
            Ok(Some(WebhookOutcome::Retrying))
        }
    }

    /// Deliver one queued lifecycle event to every configured URL, if one is
    /// ready. The body was rendered at enqueue time; each request is signed
    /// with HMAC-SHA256 over `"{timestamp}.{body}"`. The event counts as
    /// delivered when every URL answered 2xx; otherwise it is retried with
    /// backoff (all URLs again, so receivers dedupe on the event id) and
    /// dropped after the last attempt. Lifecycle URLs are operator
    /// configuration, so the tenant SSRF guard does not apply to them.
    pub async fn process_next_lifecycle_event(
        &self,
    ) -> Result<Option<WebhookOutcome>, sqlx::Error> {
        if !self.lifecycle_webhooks.enabled() {
            return Ok(None);
        }
        let Some(event) = self.lifecycle_queue.dequeue(&self.worker_id).await? else {
            return Ok(None);
        };
        let held = self.in_flight.lifecycle.hold(event.id);
        let secret = self.lifecycle_webhooks.secret.clone().unwrap_or_default();
        let mut failures = Vec::new();
        held.sending();
        for url in &self.lifecycle_webhooks.urls {
            let timestamp = chrono::Utc::now().timestamp();
            let signature =
                camelmailer_core::lifecycle::signature_header(&secret, timestamp, &event.payload);
            let result = self
                .http
                .post(url)
                .header("content-type", "application/json")
                .header(camelmailer_core::lifecycle::EVENT_HEADER, &event.event)
                .header(camelmailer_core::lifecycle::EVENT_ID_HEADER, &event.uuid)
                .header(camelmailer_core::lifecycle::SIGNATURE_HEADER, signature)
                .body(event.payload.clone())
                .send()
                .await;
            match result {
                Ok(response) if response.status().is_success() => {}
                Ok(response) => failures.push(format!("{url}: HTTP {}", response.status())),
                Err(error) => failures.push(format!("{url}: {error}")),
            }
        }
        if failures.is_empty() {
            owned(
                "lifecycle_events",
                event.id,
                self.lifecycle_queue
                    .complete(event.id, &self.worker_id)
                    .await?,
            );
            return Ok(Some(WebhookOutcome::Delivered));
        }
        let detail = failures.join("; ");
        if event.attempts + 1 >= WEBHOOK_MAX_ATTEMPTS {
            tracing::warn!(event = %event.event, id = %event.uuid, %detail,
                "lifecycle event given up after {WEBHOOK_MAX_ATTEMPTS} attempts");
            owned(
                "lifecycle_events",
                event.id,
                self.lifecycle_queue
                    .complete(event.id, &self.worker_id)
                    .await?,
            );
            Ok(Some(WebhookOutcome::GivenUp))
        } else {
            tracing::info!(event = %event.event, id = %event.uuid, %detail,
                "lifecycle event delivery failed, retrying");
            owned(
                "lifecycle_events",
                event.id,
                self.lifecycle_queue
                    .retry(event.id, event.attempts, &detail, &self.worker_id)
                    .await?,
            );
            Ok(Some(WebhookOutcome::Retrying))
        }
    }

    /// Test/ops helper: deliver every ready lifecycle event.
    pub async fn drain_lifecycle_events(&self) -> Result<usize, sqlx::Error> {
        let mut processed = 0;
        while self.process_next_lifecycle_event().await?.is_some() {
            processed += 1;
        }
        Ok(processed)
    }

    /// Test/ops helper: deliver every ready webhook request.
    pub async fn drain_webhooks(&self) -> Result<usize, sqlx::Error> {
        let mut processed = 0;
        while self.process_next_webhook().await?.is_some() {
            processed += 1;
        }
        Ok(processed)
    }

    /// Apply data retention once: per server (with its organization's
    /// overrides) message content, message metadata, the webhook log and
    /// DMARC reports; installation-wide the auth audit log and the API
    /// request log; and, always, expired authentication tokens. Every class
    /// is deleted in batches of `retention.batch_size`. A failing class is
    /// logged and the pass moves on.
    pub async fn apply_retention(&self) -> Result<camelmailer_db::RetentionReport, sqlx::Error> {
        let now = chrono::Utc::now();
        let batch = self.retention_batch;
        let mut report = camelmailer_db::RetentionReport::default();
        for target in self.store.retention_targets().await? {
            let periods = self.retention.with_overrides(&target.overrides);
            let server = target.server_id;
            if let Some(cutoff) =
                camelmailer_core::retention::cutoff(now, periods.message_metadata_days)
            {
                match self
                    .store
                    .prune_message_metadata(server, cutoff, batch)
                    .await
                {
                    Ok(n) => report.messages_deleted += n,
                    Err(error) => tracing::error!(%error, server, "message retention failed"),
                }
            }
            if let Some(cutoff) =
                camelmailer_core::retention::cutoff(now, periods.message_content_days)
            {
                match self
                    .store
                    .purge_message_content(server, cutoff, batch)
                    .await
                {
                    Ok(n) => report.message_content_purged += n,
                    Err(error) => tracing::error!(%error, server, "content retention failed"),
                }
            }
            if let Some(cutoff) = camelmailer_core::retention::cutoff(now, periods.webhook_log_days)
            {
                match self.store.prune_webhook_log(server, cutoff, batch).await {
                    Ok(n) => report.webhook_log_deleted += n,
                    Err(error) => tracing::error!(%error, server, "webhook log retention failed"),
                }
            }
            if let Some(cutoff) =
                camelmailer_core::retention::cutoff(now, periods.dmarc_report_days)
            {
                match self.store.prune_dmarc_reports(server, cutoff, batch).await {
                    Ok(n) => report.dmarc_reports_deleted += n,
                    Err(error) => tracing::error!(%error, server, "DMARC retention failed"),
                }
            }
        }
        if self.auth_event_days > 0 {
            let cutoff = now - chrono::Duration::days(self.auth_event_days as i64);
            report.auth_events_deleted = self.store.prune_auth_events(cutoff, batch).await?;
        }
        if self.api_request_days > 0 {
            let cutoff = now - chrono::Duration::days(self.api_request_days as i64);
            report.api_requests_deleted = self.store.prune_api_request_log(cutoff, batch).await?;
        }
        report.expired_tokens_deleted = self.store.prune_expired_auth_tokens(now, batch).await?;
        Ok(report)
    }

    /// Housekeeping: data retention ([`Worker::apply_retention`]), expired
    /// send-idempotency responses, send-limit counter buckets that have left
    /// every window, and the lifecycle outbox when nothing can deliver it.
    /// Returns how many API-request rows were removed. Runs periodically
    /// from [`Worker::run`].
    pub async fn housekeep(&self) -> Result<u64, camelmailer_core::StoreError> {
        let now = chrono::Utc::now();
        let expired_idempotency =
            camelmailer_core::ServerStore::prune_idempotency_requests(&self.store, now).await?;
        if expired_idempotency > 0 {
            tracing::info!(
                pruned = expired_idempotency,
                "pruned expired send-idempotency responses"
            );
        }

        let removed = match self.apply_retention().await {
            Ok(report) => {
                if !report.is_empty() {
                    tracing::info!(?report, "data retention applied");
                }
                report.api_requests_deleted
            }
            Err(error) => {
                tracing::error!(%error, "data retention error");
                0
            }
        };

        // Send-limit counter buckets that have left every 30-day window.
        // Failing to prune them is a housekeeping annoyance rather than a
        // reason to abandon the rest of the pass, so it only logs.
        match self.sink.prune_send_counters().await {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "pruned expired send-limit counters"),
            Err(error) => tracing::error!(%error, "send-counter pruning error"),
        }

        // Warm-up: judge yesterday, and prune usage and the decision log.
        match self.evaluate_warmup(now).await {
            Ok(0) => {}
            Ok(decisions) => tracing::info!(decisions, "warm-up evaluated"),
            Err(error) => tracing::error!(%error, "warm-up evaluation error"),
        }
        if let Err(error) = camelmailer_core::WarmupStore::prune_warmup(
            &self.store,
            now.date_naive() - chrono::Duration::days(WARMUP_USAGE_DAYS),
            now - chrono::Duration::days(DELIVERY_STATS_DAYS),
        )
        .await
        {
            tracing::error!(%error, "warm-up pruning error");
        }

        // Delivery counters older than DELIVERY_STATS_DAYS, and the record
        // of which address sent a message once a complaint about it can no
        // longer be expected (MESSAGE_SOURCE_DAYS).
        match DeliveryStatsStore::prune_delivery_stats(
            &self.store,
            now.date_naive() - chrono::Duration::days(DELIVERY_STATS_DAYS),
            now - chrono::Duration::days(MESSAGE_SOURCE_DAYS),
        )
        .await
        {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "pruned delivery counters"),
            Err(error) => tracing::error!(%error, "delivery-counter pruning error"),
        }

        // Without a lifecycle URL nothing can deliver the outbox (usage
        // threshold events are written by the insert transaction whether or
        // not anyone listens), so keep it from growing.
        if !self.lifecycle_webhooks.enabled() {
            match self.lifecycle_queue.discard_all().await {
                Ok(0) => {}
                Ok(dropped) => tracing::info!(
                    dropped,
                    "dropped lifecycle events: no lifecycle_webhooks.urls configured"
                ),
                Err(error) => tracing::error!(%error, "lifecycle outbox pruning error"),
            }
        }
        Ok(removed)
    }

    /// An extra delivery loop: deliver queued messages, poll when there are
    /// none. Returns once `stop` turns true and the current message is done.
    async fn deliver_loop(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        while !*stop.borrow() {
            match catch_panic(self.process_next()).await {
                Ok(Ok(Some(outcome))) => tracing::debug!(?outcome, "processed queued message"),
                Ok(Ok(None)) => pause(&mut stop, Duration::from_secs(1)).await,
                Ok(Err(error)) => {
                    tracing::error!(%error, "queue processing error");
                    pause(&mut stop, Duration::from_secs(5)).await;
                }
                Err(panic) => {
                    tracing::error!(%panic, "queue processing panicked");
                    self.defer_panicked().await;
                    pause(&mut stop, Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// The main loop: housekeeping once at startup and then hourly, one
    /// message, one webhook, one lifecycle event per round. Each step checks
    /// `stop` first, so no new work starts after a shutdown request; returns
    /// once the step running then is done.
    async fn main_loop(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        let mut last_housekeeping: Option<std::time::Instant> = None;
        while !*stop.borrow() {
            if last_housekeeping.is_none_or(|at| at.elapsed() >= HOUSEKEEPING_INTERVAL) {
                last_housekeeping = Some(std::time::Instant::now());
                match catch_panic(self.housekeep()).await {
                    Ok(Ok(0)) => {}
                    Ok(Ok(removed)) => {
                        tracing::info!(removed, "pruned expired API request-log entries")
                    }
                    Ok(Err(error)) => tracing::error!(%error, "housekeeping error"),
                    Err(panic) => tracing::error!(%panic, "housekeeping panicked"),
                }
            }
            if *stop.borrow() {
                break;
            }
            let mut idle = true;
            match catch_panic(self.process_next()).await {
                Ok(Ok(Some(outcome))) => {
                    idle = false;
                    tracing::debug!(?outcome, "processed queued message");
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => tracing::error!(%error, "queue processing error"),
                Err(panic) => {
                    tracing::error!(%panic, "queue processing panicked");
                    self.defer_panicked().await;
                }
            }
            if *stop.borrow() {
                break;
            }
            match catch_panic(self.process_next_webhook()).await {
                Ok(Ok(Some(outcome))) => {
                    idle = false;
                    tracing::debug!(?outcome, "processed webhook request");
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => tracing::error!(%error, "webhook processing error"),
                Err(panic) => {
                    tracing::error!(%panic, "webhook processing panicked");
                    self.defer_panicked().await;
                }
            }
            if *stop.borrow() {
                break;
            }
            match catch_panic(self.process_next_lifecycle_event()).await {
                Ok(Ok(Some(outcome))) => {
                    idle = false;
                    tracing::debug!(?outcome, "processed lifecycle event");
                }
                Ok(Ok(None)) => {}
                Ok(Err(error)) => tracing::error!(%error, "lifecycle event processing error"),
                Err(panic) => {
                    tracing::error!(%panic, "lifecycle event processing panicked");
                    self.defer_panicked().await;
                }
            }
            if idle {
                pause(&mut stop, Duration::from_secs(5)).await;
            }
        }
    }

    /// Refresh the in-flight locks every `heartbeat_interval` until aborted.
    async fn heartbeat_loop(&self) {
        let mut ticker = tokio::time::interval(self.heartbeat_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(error) = self.heartbeat().await {
                tracing::warn!(%error, "could not refresh the locks of in-flight rows");
            }
        }
    }

    /// The long-running worker: runs until SIGTERM or SIGINT, then shuts
    /// down gracefully (see [`Worker::run_until`]).
    pub async fn run(self: Arc<Self>) -> Result<(), LoopFailed> {
        self.run_until(shutdown_signal()).await
    }

    /// The long-running worker loop, until `shutdown` resolves. Runs
    /// housekeeping once at startup and then hourly. With `threads` above 1,
    /// further loops deliver queued messages alongside; webhooks, lifecycle
    /// events and housekeeping stay on the main loop. A heartbeat keeps the
    /// locks of in-flight rows fresh.
    ///
    /// A panic while processing one row does not stop a loop: the row is put
    /// back with one attempt more and a wait of at least the lease.
    ///
    /// On shutdown the loops stop taking new work, and the worker waits up to
    /// `worker.shutdown_grace_seconds` for what is in flight. Whatever is
    /// still running then is stopped. Rows stopped before they reached a
    /// receiver go back to the queue without an attempt being counted; every
    /// other row keeps its lock until the lease runs out, exactly as after a
    /// crash, so nothing a receiver may already have is sent again early. A
    /// loop or the heartbeat ending on its own (a bug) shuts the worker down
    /// the same way and returns an error, so the process manager restarts it.
    pub async fn run_until(
        self: Arc<Self>,
        shutdown: impl Future<Output = ()>,
    ) -> Result<(), LoopFailed> {
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let mut loops = tokio::task::JoinSet::new();
        {
            let worker = Arc::clone(&self);
            let stop = stop_rx.clone();
            loops.spawn(async move { worker.main_loop(stop).await });
        }
        for _ in 1..self.threads {
            let worker = Arc::clone(&self);
            let stop = stop_rx.clone();
            loops.spawn(async move { worker.deliver_loop(stop).await });
        }
        let mut heartbeat = tokio::task::JoinSet::new();
        {
            let worker = Arc::clone(&self);
            heartbeat.spawn(async move { worker.heartbeat_loop().await });
        }
        tracing::info!(worker_id = %self.worker_id, threads = self.threads, "camelmailer worker started");

        let mut failure = None;
        tokio::select! {
            _ = shutdown => {
                tracing::info!(grace = ?self.shutdown_grace, "shutdown requested; finishing in-flight work");
            }
            Some(ended) = loops.join_next() => {
                let detail = match ended {
                    Ok(()) => "a worker loop ended".to_string(),
                    Err(error) => format!("a worker loop failed: {error}"),
                };
                tracing::error!(%detail, "stopping the worker");
                failure = Some(detail);
            }
            Some(ended) = heartbeat.join_next() => {
                // Without its heartbeat the worker would lose long deliveries
                // to other workers once the lease runs out.
                let detail = match ended {
                    Ok(()) => "the lock heartbeat ended".to_string(),
                    Err(error) => format!("the lock heartbeat failed: {error}"),
                };
                tracing::error!(%detail, "stopping the worker");
                failure = Some(detail);
            }
        }
        let _ = stop_tx.send(true);

        let drained = tokio::time::timeout(self.shutdown_grace, async {
            while loops.join_next().await.is_some() {}
        })
        .await
        .is_ok();
        if !drained {
            tracing::warn!("in-flight work did not finish within the grace period; stopping it");
            // Hand back what has not reached a receiver yet while it is
            // still marked in flight; aborting drops those marks.
            match self.release_unstarted().await {
                Ok(released) => tracing::info!(?released, "unstarted rows released"),
                Err(error) => {
                    tracing::error!(%error, "could not release unstarted rows; they return after the lock lease")
                }
            }
            loops.abort_all();
            while loops.join_next().await.is_some() {}
        }
        heartbeat.abort_all();
        while heartbeat.join_next().await.is_some() {}
        tracing::info!("worker stopped; rows still locked return after the lock lease");

        match failure {
            Some(detail) => Err(LoopFailed(detail)),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_panic_while_processing_a_row_is_caught_and_the_row_marked() {
        let tracked = Tracked::default();
        let result = catch_panic(async {
            let _held = tracked.hold(7);
            tokio::task::yield_now().await;
            if tracked.contains(7) {
                panic!("poison message");
            }
        })
        .await;
        assert_eq!(result, Err("poison message".to_string()));
        assert!(!tracked.contains(7), "no longer in flight");
        assert_eq!(tracked.take_panicked(), vec![7]);
        assert!(tracked.take_panicked().is_empty());
    }

    #[tokio::test]
    async fn a_row_that_returns_normally_is_not_marked_as_panicked() {
        let tracked = Tracked::default();
        let result = catch_panic(async {
            let _held = tracked.hold(3);
            42
        })
        .await;
        assert_eq!(result, Ok(42));
        assert!(!tracked.contains(3));
        assert!(tracked.take_panicked().is_empty());
    }

    #[test]
    fn only_rows_that_have_not_reached_a_receiver_count_as_unstarted() {
        let tracked = Tracked::default();
        let preparing = tracked.hold(1);
        let sending = tracked.hold(2);
        sending.sending();
        tracked.hold(3).sending();
        assert_eq!(tracked.take_unstarted(), vec![1]);
        let mut all = tracked.ids();
        all.sort_unstable();
        assert_eq!(all, vec![1, 2]);
        // Taken once: a released row is not handed back twice.
        assert!(tracked.take_unstarted().is_empty());
        drop(preparing);
        assert!(tracked.take_unstarted().is_empty());
    }

    #[test]
    fn a_delivery_counts_as_unstarted_until_its_client_enters_the_data_gate() {
        let tracked = Tracked::default();
        let _before = tracked.hold(1);
        let before_gate = tracked.delivering(1);
        let _after = tracked.hold(2);
        let after_gate = tracked.delivering(2);
        assert!(after_gate.enter(), "an open gate lets DATA through");

        assert_eq!(tracked.take_unstarted(), vec![1]);
        assert!(!before_gate.enter(), "a released delivery never sends DATA");
        assert!(after_gate.data_started());
        assert!(after_gate.enter(), "a later endpoint may send DATA again");
    }

    #[test]
    fn a_row_released_while_preparing_gets_a_closed_gate() {
        let tracked = Tracked::default();
        let _held = tracked.hold(7);
        assert_eq!(tracked.take_unstarted(), vec![7]);
        let gate = tracked.delivering(7);
        assert!(!gate.enter());
        tracked.sending(7);
        assert!(tracked.take_unstarted().is_empty());
    }
}

#[cfg(test)]
mod source_selection_tests {
    use super::choose_source_address;
    use camelmailer_core::IpAddress;

    fn address(id: u64, ipv4: &str, priority: i32) -> IpAddress {
        IpAddress {
            id: id as camelmailer_core::Id,
            uuid: format!("u{id}"),
            ip_pool_id: 1,
            ipv4: ipv4.into(),
            ipv6: None,
            hostname: format!("out{id}.example"),
            priority,
        }
    }

    #[test]
    fn the_lowest_priority_number_wins() {
        let a = address(1, "203.0.113.1", 20);
        let b = address(2, "203.0.113.2", 10);
        let chosen = choose_source_address(&[&b, &a], "gmail.com").unwrap();
        assert_eq!(chosen.ipv4, "203.0.113.2");
    }

    #[test]
    fn ties_spread_by_recipient_domain_and_stay_stable() {
        let a = address(1, "203.0.113.1", 10);
        let b = address(2, "203.0.113.2", 10);
        let local = [&a, &b];
        let mut seen = std::collections::HashSet::new();
        for domain in [
            "gmail.com",
            "outlook.com",
            "web.de",
            "gmx.de",
            "t-online.de",
            "yahoo.com",
        ] {
            let first = choose_source_address(&local, domain).unwrap().ipv4.clone();
            // Same domain, same address, whatever the case.
            let again = choose_source_address(&local, &domain.to_uppercase()).unwrap();
            assert_eq!(first, again.ipv4);
            seen.insert(first);
        }
        assert_eq!(seen.len(), 2, "both tied addresses get used");
    }

    #[test]
    fn no_local_address_chooses_nothing() {
        assert!(choose_source_address(&[], "gmail.com").is_none());
    }
}
