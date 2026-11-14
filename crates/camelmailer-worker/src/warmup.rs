//! The worker's side of the adaptive warm-up (see
//! [`camelmailer_core::warmup`]): which pool addresses warm up, the choice
//! of an address with room, the brake during the day and the daily
//! evaluation.
//!
//! The set of warming addresses is re-read at most once a minute. While it
//! is empty (the default: no pool and no address has a warm-up mode) the
//! worker never consults warm-up, so source selection and delivery are
//! exactly as without it.
//!
//! When the warm-up storage cannot be read, an address the worker last saw
//! fully warm for the provider (or with warm-up off) keeps sending; mail
//! that would go out from a not-yet-warm address waits, without an attempt
//! counted, until the storage answers again. A storage error never lets a
//! cold address send without its limit. The outage is logged as an error
//! (at most once a minute) and, once over, written to the warm-up log of
//! every address whose mail waited.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use camelmailer_core::{
    Id, IpAddress, IpDeliveryStat, ProviderGroup, StoreError, WarmupLogEntry, WarmupProfile,
    WarmupProfiles, WarmupSignal, WarmupState, WarmupStatus, WarmupStore, WarmupThresholds,
};
use chrono::{DateTime, NaiveDate, Utc};

/// How often the set of warming addresses is re-read.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// How long mail waits while the warm-up storage cannot be read.
pub const STORAGE_RETRY_SECONDS: i64 = 60;

/// How often a storage outage is logged as an error.
const OUTAGE_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// The built-in profiles with the `warmup.profiles` changes applied (the
/// API has the same conversion in `camelmailer_api::warmup_config`; the API
/// crate is not a dependency of the worker library).
pub fn profiles_from_config(config: &camelmailer_config::Warmup) -> WarmupProfiles {
    let mut profiles = WarmupProfiles::default();
    for (name, changes) in &config.profiles {
        let groups: Vec<(String, i64, i64)> = changes
            .groups
            .iter()
            .map(|(group, limits)| (group.clone(), limits.start, limits.cap))
            .collect();
        profiles.override_profile(name, changes.start, changes.growth, changes.cap, &groups);
    }
    profiles
}

/// The thresholds of the `warmup` config group.
pub fn thresholds_from(config: &camelmailer_config::Warmup) -> WarmupThresholds {
    WarmupThresholds {
        min_volume_fraction: config.min_volume_fraction,
        advance_fraction: config.advance_fraction,
        deferral_rate_slow: config.deferral_rate_slow,
        bounce_rate_brake: config.bounce_rate_brake,
        policy_blocks_brake: config.policy_blocks_brake,
        policy_block_rate_brake: config.policy_block_rate_brake,
        complaint_rate_warn: config.complaint_rate_warn,
        complaint_rate_brake: config.complaint_rate_brake,
        pause_hours: config.pause_hours,
        step_back: config.step_back,
    }
}

/// How old (in days before the judged day) a provider signal may be and
/// still count. Postmaster Tools and SNDS publish with a delay of a day or
/// two; older data says little about the address today.
pub const SIGNAL_MAX_AGE_DAYS: i64 = 3;

/// The provider feedback that applies to one address and provider, as
/// warm-up signals: `bad` brakes, `warn` slows, `ok` lets a healthy day
/// advance (the decision reason names the sources). Readings without a
/// verdict (`unknown`) and readings older than [`SIGNAL_MAX_AGE_DAYS`]
/// before `day` are left out.
pub fn warmup_signals(
    found: &[camelmailer_core::ProviderSignal],
    day: NaiveDate,
) -> Vec<WarmupSignal> {
    use camelmailer_core::{SignalVerdict, Verdict};
    found
        .iter()
        .filter(|signal| signal.data_date >= day - chrono::Duration::days(SIGNAL_MAX_AGE_DAYS))
        .filter_map(|signal| {
            let verdict = match signal.verdict {
                Verdict::Unknown => return None,
                Verdict::Ok => SignalVerdict::Ok,
                Verdict::Warn => SignalVerdict::Warn,
                Verdict::Bad => SignalVerdict::Bad,
            };
            Some(WarmupSignal {
                source: signal.source.clone(),
                verdict,
                detail: format!(
                    "{} {}: {} (data of {})",
                    signal.subject_kind, signal.subject, signal.detail, signal.data_date
                ),
            })
        })
        .collect()
}

/// What warm-up says about a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Send from this address.
    Use(IpAddress),
    /// No address of the pool has room for this provider today.
    Wait,
    /// The warm-up storage could not be read and no address the worker
    /// knows to be warm is available: wait until it can be read.
    WaitForStorage,
}

#[derive(Default)]
struct Active {
    /// Warming address id -> its profile.
    profiles: HashMap<Id, WarmupProfile>,
    /// (address, provider) pairs last seen fully warm.
    warm: HashSet<(Id, ProviderGroup)>,
    /// When the settings were last read successfully; `None` until then.
    loaded_at: Option<Instant>,
    /// When the last read was tried.
    tried_at: Option<Instant>,
}

/// A warm-up storage outage in progress.
struct Outage {
    since: DateTime<Utc>,
    errors: u64,
    /// Messages that waited, per address they would have gone out from.
    waited: HashMap<Id, u64>,
    last_logged: Option<Instant>,
}

/// Counters of warm-up storage outages since the worker started.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StorageHealth {
    /// The storage is failing right now.
    pub failing: bool,
    /// Storage errors seen.
    pub errors: u64,
    /// Messages that waited because of them.
    pub waited: u64,
}

pub struct WarmupGate {
    pub thresholds: WarmupThresholds,
    /// How long a message waits when no address has room (`warmup.defer_seconds`).
    pub defer_seconds: i64,
    profiles: WarmupProfiles,
    active: Mutex<Active>,
    outage: Mutex<Option<Outage>>,
    health: Mutex<StorageHealth>,
}

impl WarmupGate {
    pub fn new(config: &camelmailer_config::Warmup) -> Self {
        Self {
            thresholds: thresholds_from(config),
            defer_seconds: i64::from(config.defer_seconds),
            profiles: profiles_from_config(config),
            active: Mutex::new(Active::default()),
            outage: Mutex::new(None),
            health: Mutex::new(StorageHealth::default()),
        }
    }

    /// The built-in profiles as configured.
    pub fn profiles(&self) -> &WarmupProfiles {
        &self.profiles
    }

    /// Outage counters since the worker started.
    pub fn storage_health(&self) -> StorageHealth {
        *self.health.lock().unwrap()
    }

    /// Re-read the warming addresses when the last read is older than a
    /// minute (or failed: then at most every few seconds).
    pub async fn refresh(&self, store: &dyn WarmupStore) -> Result<(), StoreError> {
        let due = {
            let active = self.active.lock().unwrap();
            match (active.loaded_at, active.tried_at) {
                (Some(loaded), Some(tried)) if tried <= loaded => {
                    loaded.elapsed() >= REFRESH_INTERVAL
                }
                (_, Some(tried)) => tried.elapsed() >= Duration::from_secs(5),
                (_, None) => true,
            }
        };
        if !due {
            return Ok(());
        }
        self.reload(store).await
    }

    /// Re-read the warming addresses (and which of them are warm) now. A
    /// failed read keeps what was read before.
    pub async fn reload(&self, store: &dyn WarmupStore) -> Result<(), StoreError> {
        self.active.lock().unwrap().tried_at = Some(Instant::now());
        let loaded = async {
            Ok::<_, StoreError>((
                store.warmup_addresses().await?,
                store.warmup_states().await?,
            ))
        }
        .await;
        let (addresses, states) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.storage_failed(&error);
                return Err(error);
            }
        };
        {
            let mut active = self.active.lock().unwrap();
            active.profiles = addresses
                .iter()
                .filter_map(|a| {
                    a.effective_profile(&self.profiles)
                        .map(|p| (a.ip_address_id, p))
                })
                .collect();
            active.warm = states
                .iter()
                .filter(|s| s.status == WarmupStatus::Warm)
                .map(|s| (s.ip_address_id, s.provider.clone()))
                .collect();
            active.loaded_at = Some(Instant::now());
        }
        self.storage_recovered(store).await;
        Ok(())
    }

    /// The profile of a warming address.
    pub fn profile(&self, ip_address_id: Id) -> Option<WarmupProfile> {
        self.active
            .lock()
            .unwrap()
            .profiles
            .get(&ip_address_id)
            .cloned()
    }

    /// Whether any of `candidates` warms up, or might (the settings were
    /// never read). False whenever warm-up is known to be off everywhere,
    /// so the caller leaves source selection alone.
    pub fn covers(&self, candidates: &[IpAddress]) -> bool {
        let active = self.active.lock().unwrap();
        if active.loaded_at.is_none() {
            // Nothing known yet: every address may be cold.
            return !candidates.is_empty() && active.tried_at.is_some();
        }
        !active.profiles.is_empty()
            && candidates
                .iter()
                .any(|a| active.profiles.contains_key(&a.id))
    }

    /// Choose the address a message to `provider` goes out from: `chosen`
    /// (what source selection picked) if it has room, else the next of
    /// `candidates` (lowest priority number, then id) that has. An address
    /// that does not warm up always has room. When the storage cannot be
    /// read, only an address known to be warm (or without warm-up) is used.
    pub async fn pick(
        &self,
        store: &dyn WarmupStore,
        chosen: &IpAddress,
        candidates: &[IpAddress],
        provider: &ProviderGroup,
        now: DateTime<Utc>,
    ) -> Pick {
        let mut order: Vec<&IpAddress> = vec![chosen];
        let mut rest: Vec<&IpAddress> = candidates.iter().filter(|a| a.id != chosen.id).collect();
        rest.sort_by_key(|a| (a.priority, a.id));
        order.extend(rest);
        let known = self.active.lock().unwrap().loaded_at.is_some();
        let mut storage_failed = false;
        let mut first_cold: Option<Id> = None;
        for address in order {
            let profile = self.profile(address.id);
            let Some(profile) = profile else {
                if known {
                    return Pick::Use(address.clone());
                }
                // Settings never read: the address may be warming up.
                first_cold.get_or_insert(address.id);
                storage_failed = true;
                continue;
            };
            let ramp = profile.ramp(provider);
            let initial = WarmupState::initial(address.id, provider.clone(), &ramp);
            match store
                .try_reserve_warmup(
                    address.id,
                    provider.clone(),
                    now.date_naive(),
                    &initial,
                    now,
                )
                .await
            {
                Ok(true) => {
                    self.storage_recovered(store).await;
                    return Pick::Use(address.clone());
                }
                Ok(false) => {
                    self.storage_recovered(store).await;
                    continue;
                }
                Err(error) => {
                    self.storage_failed(&error);
                    let warm = self
                        .active
                        .lock()
                        .unwrap()
                        .warm
                        .contains(&(address.id, provider.clone()));
                    if warm {
                        return Pick::Use(address.clone());
                    }
                    first_cold.get_or_insert(address.id);
                    storage_failed = true;
                }
            }
        }
        if storage_failed {
            if let Some(id) = first_cold {
                self.note_waited(id);
            }
            Pick::WaitForStorage
        } else {
            Pick::Wait
        }
    }

    fn storage_failed(&self, error: &StoreError) {
        let mut outage = self.outage.lock().unwrap();
        let current = outage.get_or_insert_with(|| Outage {
            since: Utc::now(),
            errors: 0,
            waited: HashMap::new(),
            last_logged: None,
        });
        current.errors += 1;
        let mut health = self.health.lock().unwrap();
        health.failing = true;
        health.errors += 1;
        if current
            .last_logged
            .is_none_or(|at| at.elapsed() >= OUTAGE_LOG_INTERVAL)
        {
            current.last_logged = Some(Instant::now());
            tracing::error!(
                %error,
                since = %current.since.to_rfc3339(),
                errors = current.errors,
                waited = current.waited.values().sum::<u64>(),
                "warm-up storage unavailable; mail from not-yet-warm addresses waits without an attempt"
            );
        }
    }

    fn note_waited(&self, ip_address_id: Id) {
        if let Some(outage) = self.outage.lock().unwrap().as_mut() {
            *outage.waited.entry(ip_address_id).or_insert(0) += 1;
        }
        self.health.lock().unwrap().waited += 1;
    }

    /// The storage answered again: end the outage, if there was one, and
    /// record it in the warm-up log of every address whose mail waited.
    async fn storage_recovered(&self, store: &dyn WarmupStore) {
        let Some(outage) = self.outage.lock().unwrap().take() else {
            return;
        };
        self.health.lock().unwrap().failing = false;
        let now = Utc::now();
        tracing::warn!(
            since = %outage.since.to_rfc3339(),
            errors = outage.errors,
            waited = outage.waited.values().sum::<u64>(),
            "warm-up storage available again"
        );
        for (ip_address_id, waited) in &outage.waited {
            let entry = WarmupLogEntry {
                ip_address_id: *ip_address_id,
                provider: None,
                decision: "storage".into(),
                reason: format!(
                    "warm-up storage was unreadable from {} to {}; {waited} messages from this not-yet-warm address waited without an attempt",
                    outage.since.to_rfc3339(),
                    now.to_rfc3339()
                ),
                allowance_before: None,
                allowance_after: None,
                created_at: now,
            };
            if let Err(error) = store.log_warmup(&entry).await {
                tracing::warn!(%error, "could not log the warm-up storage outage");
            }
        }
    }

    /// After a policy refusal: brake at once when today's refusals from the
    /// address to the provider reach the brake limits. Returns the reason
    /// when it braked.
    pub async fn brake_if_blocked(
        &self,
        store: &dyn WarmupStore,
        ip_address_id: Id,
        provider: &ProviderGroup,
        today: &IpDeliveryStat,
        now: DateTime<Utc>,
    ) -> Result<Option<String>, StoreError> {
        let Some(profile) = self.profile(ip_address_id) else {
            return Ok(None);
        };
        let Some(state) = store
            .warmup_states()
            .await?
            .into_iter()
            .find(|s| s.ip_address_id == ip_address_id && &s.provider == provider)
        else {
            return Ok(None);
        };
        let ramp = profile.ramp(provider);
        let Some(braked) =
            camelmailer_core::warmup::intraday_brake(&state, &ramp, today, &self.thresholds, now)
        else {
            return Ok(None);
        };
        let entry = WarmupLogEntry {
            ip_address_id,
            provider: Some(provider.clone()),
            decision: "brake".into(),
            reason: braked.last_reason.clone(),
            allowance_before: Some(state.allowance),
            allowance_after: Some(braked.allowance),
            created_at: now,
        };
        if store.save_warmup_state(&braked, Some(&entry)).await? {
            tracing::warn!(ip_address_id, provider = provider.as_str(), reason = %braked.last_reason, "warm-up brake");
            Ok(Some(braked.last_reason))
        } else {
            Ok(None)
        }
    }

    /// Judge `day` for every warming address and provider it has a state
    /// for, once (a state already judged for `day` or later is skipped, and
    /// a concurrent worker that saved first wins). `stats` are that day's
    /// counters; `signals` returns the outside verdicts for an address and
    /// provider. Returns the log entries written.
    pub async fn evaluate_day(
        &self,
        store: &dyn WarmupStore,
        day: NaiveDate,
        stats: &[IpDeliveryStat],
        addresses: &HashMap<Id, String>,
        signals: &(dyn Fn(Id, &ProviderGroup) -> Vec<WarmupSignal> + Send + Sync),
        now: DateTime<Utc>,
    ) -> Result<Vec<WarmupLogEntry>, StoreError> {
        let mut written = Vec::new();
        for state in store.warmup_states().await? {
            let Some(profile) = self.profile(state.ip_address_id) else {
                continue;
            };
            if state.evaluated_day.is_some_and(|judged| judged >= day) {
                continue;
            }
            let ramp = profile.ramp(&state.provider);
            let ipv4 = addresses.get(&state.ip_address_id);
            let stat = stats.iter().find(|s| {
                s.day == day
                    && s.provider == state.provider
                    && (s.ip_address_id == Some(state.ip_address_id)
                        || ipv4.is_some_and(|ip| &s.source_ip == ip))
            });
            let found = signals(state.ip_address_id, &state.provider);
            let (next, decision) = camelmailer_core::warmup::evaluate(
                &state,
                &ramp,
                day,
                stat,
                &found,
                &self.thresholds,
                now,
            );
            let entry = WarmupLogEntry {
                ip_address_id: state.ip_address_id,
                provider: Some(state.provider.clone()),
                decision: decision.as_str().into(),
                reason: next.last_reason.clone(),
                allowance_before: Some(state.allowance),
                allowance_after: Some(next.allowance),
                created_at: now,
            };
            if store.save_warmup_state(&next, Some(&entry)).await? {
                tracing::info!(
                    ip_address_id = state.ip_address_id,
                    provider = state.provider.as_str(),
                    decision = decision.as_str(),
                    reason = %next.last_reason,
                    "warm-up decision"
                );
                written.push(entry);
            }
        }
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camelmailer_core::{AdminStore, MemoryStore, NewIpAddress, WarmupMode};

    async fn pool_with(store: &MemoryStore, count: usize) -> Vec<IpAddress> {
        let pool = store.create_ip_pool("Main", true).await.unwrap();
        let mut addresses = Vec::new();
        for i in 0..count {
            addresses.push(
                store
                    .create_ip_address(NewIpAddress {
                        ip_pool_id: pool.id,
                        ipv4: format!("203.0.113.{}", 10 + i),
                        ipv6: None,
                        hostname: format!("out{}.example.com", i + 1),
                        priority: 100,
                    })
                    .await
                    .unwrap(),
            );
        }
        addresses
    }

    #[test]
    fn provider_feedback_becomes_warmup_signals_when_fresh_and_judged() {
        use camelmailer_core::{ProviderSignal, SignalVerdict, Verdict};
        let day = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let signal = |source: &str, verdict, data_date| ProviderSignal {
            source: source.into(),
            subject_kind: "ip".into(),
            subject: "203.0.113.5".into(),
            verdict,
            detail: "filter result RED".into(),
            data_date,
            observed_at: Utc::now(),
        };
        let found = vec![
            signal("snds", Verdict::Bad, day),
            signal("postmaster_tools", Verdict::Warn, day.pred_opt().unwrap()),
            signal("fbl", Verdict::Unknown, day),
            signal("placement", Verdict::Ok, day),
            signal("snds", Verdict::Bad, day - chrono::Duration::days(10)),
        ];
        let signals = warmup_signals(&found, day);
        assert_eq!(
            signals
                .iter()
                .map(|s| (s.source.as_str(), s.verdict))
                .collect::<Vec<_>>(),
            vec![
                ("snds", SignalVerdict::Bad),
                ("postmaster_tools", SignalVerdict::Warn),
                ("placement", SignalVerdict::Ok)
            ]
        );
        assert!(signals[0]
            .detail
            .contains("ip 203.0.113.5: filter result RED"));
    }

    #[tokio::test]
    async fn with_warmup_off_everywhere_the_gate_covers_nothing() {
        let store = MemoryStore::new();
        let addresses = pool_with(&store, 2).await;
        let gate = WarmupGate::new(&Default::default());
        gate.reload(&store).await.unwrap();
        assert!(!gate.covers(&addresses));
    }

    #[tokio::test]
    async fn volume_beyond_the_allowance_moves_to_an_address_with_room_then_waits() {
        let store = MemoryStore::new();
        let addresses = pool_with(&store, 2).await;
        let custom = WarmupProfile::uniform(2, 2.0, 100);
        store
            .set_address_warmup(
                addresses[0].id,
                Some(WarmupMode::Custom),
                Some(custom.clone()),
            )
            .await
            .unwrap();
        store
            .set_address_warmup(
                addresses[1].id,
                Some(WarmupMode::Custom),
                Some(custom.clone()),
            )
            .await
            .unwrap();
        let gate = WarmupGate::new(&Default::default());
        gate.reload(&store).await.unwrap();
        assert!(gate.covers(&addresses));
        let now = Utc::now();
        let mut picks = Vec::new();
        for _ in 0..5 {
            picks.push(
                gate.pick(
                    &store,
                    &addresses[0],
                    &addresses,
                    &ProviderGroup::microsoft(),
                    now,
                )
                .await,
            );
        }
        assert_eq!(picks[0], Pick::Use(addresses[0].clone()));
        assert_eq!(picks[1], Pick::Use(addresses[0].clone()));
        assert_eq!(
            picks[2],
            Pick::Use(addresses[1].clone()),
            "overflow to the next address"
        );
        assert_eq!(picks[3], Pick::Use(addresses[1].clone()));
        assert_eq!(picks[4], Pick::Wait, "no address has room");
        // Google has its own allowance.
        assert_eq!(
            gate.pick(
                &store,
                &addresses[0],
                &addresses,
                &ProviderGroup::google(),
                now
            )
            .await,
            Pick::Use(addresses[0].clone())
        );
    }

    #[tokio::test]
    async fn an_address_without_warmup_in_the_pool_takes_the_overflow() {
        let store = MemoryStore::new();
        let addresses = pool_with(&store, 2).await;
        store
            .set_address_warmup(
                addresses[0].id,
                Some(WarmupMode::Custom),
                Some(WarmupProfile::uniform(1, 2.0, 100)),
            )
            .await
            .unwrap();
        let gate = WarmupGate::new(&Default::default());
        gate.reload(&store).await.unwrap();
        let now = Utc::now();
        assert_eq!(
            gate.pick(
                &store,
                &addresses[0],
                &addresses,
                &ProviderGroup::yahoo(),
                now
            )
            .await,
            Pick::Use(addresses[0].clone())
        );
        for _ in 0..3 {
            assert_eq!(
                gate.pick(
                    &store,
                    &addresses[0],
                    &addresses,
                    &ProviderGroup::yahoo(),
                    now
                )
                .await,
                Pick::Use(addresses[1].clone())
            );
        }
    }

    #[tokio::test]
    async fn the_daily_evaluation_advances_once_and_logs_why() {
        let store = MemoryStore::new();
        let addresses = pool_with(&store, 1).await;
        store
            .set_address_warmup(addresses[0].id, Some(WarmupMode::Normal), None)
            .await
            .unwrap();
        let gate = WarmupGate::new(&Default::default());
        gate.reload(&store).await.unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let google = ProviderGroup::google();
        let ramp = WarmupProfile::normal().ramp(&google);
        let initial = WarmupState::initial(addresses[0].id, google.clone(), &ramp);
        let morning = DateTime::parse_from_rfc3339("2026-11-14T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(store
            .try_reserve_warmup(addresses[0].id, google.clone(), day, &initial, morning)
            .await
            .unwrap());
        let stats = vec![IpDeliveryStat {
            day,
            source_ip: addresses[0].ipv4.clone(),
            ip_address_id: Some(addresses[0].id),
            provider: google.clone(),
            sent: 199,
            deferred: 1,
            bounced: 0,
            policy_blocks: 0,
            complaints: 0,
        }];
        let ips: HashMap<Id, String> = [(addresses[0].id, addresses[0].ipv4.clone())].into();
        let night = DateTime::parse_from_rfc3339("2026-11-15T00:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let none = |_: Id, _: &ProviderGroup| Vec::new();
        let written = gate
            .evaluate_day(&store, day, &stats, &ips, &none, night)
            .await
            .unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].decision, "advance");
        assert_eq!(written[0].allowance_after, Some(300));
        // A second run (or a second worker) does not judge the day again.
        assert!(gate
            .evaluate_day(&store, day, &stats, &ips, &none, night)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .warmup_log(Some(addresses[0].id), 10)
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.warmup_states().await.unwrap()[0].allowance, 300);
    }

    /// A store whose warm-up reads and reservations can be made to fail.
    struct Flaky {
        inner: MemoryStore,
        failing: std::sync::atomic::AtomicBool,
    }

    impl Flaky {
        fn check(&self) -> Result<(), StoreError> {
            if self.failing.load(std::sync::atomic::Ordering::SeqCst) {
                Err(StoreError::Other("connection refused".into()))
            } else {
                Ok(())
            }
        }
        fn fail(&self, failing: bool) {
            self.failing
                .store(failing, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl WarmupStore for Flaky {
        async fn warmup_addresses(
            &self,
        ) -> Result<Vec<camelmailer_core::WarmupAddress>, StoreError> {
            self.check()?;
            self.inner.warmup_addresses().await
        }
        async fn set_address_warmup(
            &self,
            id: Id,
            mode: Option<WarmupMode>,
            profile: Option<WarmupProfile>,
        ) -> Result<bool, StoreError> {
            self.inner.set_address_warmup(id, mode, profile).await
        }
        async fn set_pool_warmup(
            &self,
            id: Id,
            mode: Option<WarmupMode>,
            profile: Option<WarmupProfile>,
        ) -> Result<bool, StoreError> {
            self.inner.set_pool_warmup(id, mode, profile).await
        }
        async fn warmup_states(&self) -> Result<Vec<WarmupState>, StoreError> {
            self.check()?;
            self.inner.warmup_states().await
        }
        async fn save_warmup_state(
            &self,
            state: &WarmupState,
            entry: Option<&WarmupLogEntry>,
        ) -> Result<bool, StoreError> {
            self.check()?;
            self.inner.save_warmup_state(state, entry).await
        }
        async fn try_reserve_warmup(
            &self,
            id: Id,
            provider: ProviderGroup,
            day: NaiveDate,
            initial: &WarmupState,
            now: DateTime<Utc>,
        ) -> Result<bool, StoreError> {
            self.check()?;
            self.inner
                .try_reserve_warmup(id, provider, day, initial, now)
                .await
        }
        async fn warmup_usage(
            &self,
            day: NaiveDate,
        ) -> Result<Vec<(Id, ProviderGroup, i64)>, StoreError> {
            self.inner.warmup_usage(day).await
        }
        async fn log_warmup(&self, entry: &WarmupLogEntry) -> Result<(), StoreError> {
            self.inner.log_warmup(entry).await
        }
        async fn warmup_log(
            &self,
            id: Option<Id>,
            limit: i64,
        ) -> Result<Vec<WarmupLogEntry>, StoreError> {
            self.inner.warmup_log(id, limit).await
        }
        async fn prune_warmup(
            &self,
            usage_before: NaiveDate,
            log_before: DateTime<Utc>,
        ) -> Result<u64, StoreError> {
            self.inner.prune_warmup(usage_before, log_before).await
        }
    }

    /// Decision 1: when the warm-up storage cannot be read, an address
    /// known to be fully warm (or without warm-up) keeps sending; mail from
    /// a not-yet-warm address waits for the storage; the outage ends up in
    /// the log of the address whose mail waited.
    #[tokio::test]
    async fn a_storage_error_never_lets_a_cold_address_send() {
        let store = Flaky {
            inner: MemoryStore::new(),
            failing: false.into(),
        };
        let addresses = pool_with(&store.inner, 3).await;
        let (cold, warm, plain) = (&addresses[0], &addresses[1], &addresses[2]);
        let profile = WarmupProfile::uniform(10, 2.0, 100);
        for address in [cold, warm] {
            store
                .set_address_warmup(address.id, Some(WarmupMode::Custom), Some(profile.clone()))
                .await
                .unwrap();
        }
        let google = ProviderGroup::google();
        let now = Utc::now();
        // The warm address reached its cap for Google.
        let mut warm_state = WarmupState::initial(warm.id, google.clone(), &profile.ramp(&google));
        warm_state.allowance = 100;
        warm_state.status = WarmupStatus::Warm;
        assert!(store.save_warmup_state(&warm_state, None).await.unwrap());
        let gate = WarmupGate::new(&Default::default());
        gate.reload(&store).await.unwrap();

        store.fail(true);
        // Cold only: waits for the storage.
        assert_eq!(
            gate.pick(&store, cold, std::slice::from_ref(cold), &google, now)
                .await,
            Pick::WaitForStorage
        );
        // Cold chosen, but the pool has a known-warm address: that one sends.
        assert_eq!(
            gate.pick(&store, cold, &[cold.clone(), warm.clone()], &google, now)
                .await,
            Pick::Use(warm.clone())
        );
        // Warm for Google is not warm for Yahoo.
        assert_eq!(
            gate.pick(
                &store,
                warm,
                std::slice::from_ref(warm),
                &ProviderGroup::yahoo(),
                now
            )
            .await,
            Pick::WaitForStorage
        );
        // An address without warm-up sends as always.
        assert_eq!(
            gate.pick(&store, cold, &[cold.clone(), plain.clone()], &google, now)
                .await,
            Pick::Use(plain.clone())
        );
        let health = gate.storage_health();
        assert!(health.failing);
        assert!(health.errors >= 3);
        assert_eq!(health.waited, 2);

        // The storage answers again: the outage is logged for the cold address.
        store.fail(false);
        assert_eq!(
            gate.pick(&store, cold, std::slice::from_ref(cold), &google, now)
                .await,
            Pick::Use(cold.clone())
        );
        assert!(!gate.storage_health().failing);
        let log = store.warmup_log(Some(cold.id), 10).await.unwrap();
        assert_eq!(log[0].decision, "storage");
        assert!(
            log[0].reason.contains("waited without an attempt"),
            "{}",
            log[0].reason
        );
    }

    /// Before the settings could ever be read, every pool address may be
    /// cold: pool mail waits for the storage.
    #[tokio::test]
    async fn with_unreadable_settings_from_the_start_pool_mail_waits() {
        let store = Flaky {
            inner: MemoryStore::new(),
            failing: true.into(),
        };
        let addresses = pool_with(&store.inner, 1).await;
        let gate = WarmupGate::new(&Default::default());
        assert!(!gate.covers(&addresses), "not tried yet");
        assert!(gate.refresh(&store).await.is_err());
        assert!(gate.covers(&addresses));
        assert_eq!(
            gate.pick(
                &store,
                &addresses[0],
                &addresses,
                &ProviderGroup::other(),
                Utc::now()
            )
            .await,
            Pick::WaitForStorage
        );
        store.fail(false);
        tokio::time::sleep(Duration::from_millis(10)).await;
        gate.reload(&store).await.unwrap();
        assert!(!gate.covers(&addresses), "known now: warm-up off");
    }

    /// Each provider group has its own allowance: Microsoft starts lower
    /// than Google in the normal profile.
    #[tokio::test]
    async fn each_group_gets_its_own_first_day_allowance() {
        let store = MemoryStore::new();
        let addresses = pool_with(&store, 1).await;
        store
            .set_address_warmup(addresses[0].id, Some(WarmupMode::Normal), None)
            .await
            .unwrap();
        let gate = WarmupGate::new(&Default::default());
        gate.reload(&store).await.unwrap();
        let now = Utc::now();
        for provider in [ProviderGroup::microsoft(), ProviderGroup::t_online()] {
            assert_eq!(
                gate.pick(&store, &addresses[0], &addresses, &provider, now)
                    .await,
                Pick::Use(addresses[0].clone())
            );
        }
        let states = store.warmup_states().await.unwrap();
        let allowance = |name: &str| {
            states
                .iter()
                .find(|s| s.provider.as_str() == name)
                .unwrap()
                .allowance
        };
        assert_eq!(allowance("microsoft"), 100);
        assert_eq!(allowance("t_online"), 50);
    }
}
