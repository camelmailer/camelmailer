//! The worker's side of the adaptive warm-up (see
//! [`camelmailer_core::warmup`]): which pool addresses warm up, the choice
//! of an address with room, the brake during the day and the daily
//! evaluation.
//!
//! The set of warming addresses is re-read at most once a minute. While it
//! is empty (the default: no pool and no address has a warm-up mode) the
//! worker never consults warm-up, so source selection and delivery are
//! exactly as without it.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use camelmailer_core::{
    Id, IpAddress, IpDeliveryStat, ProviderGroup, StoreError, WarmupLogEntry, WarmupProfile,
    WarmupSignal, WarmupState, WarmupStore, WarmupThresholds,
};
use chrono::{DateTime, NaiveDate, Utc};

/// How often the set of warming addresses is re-read.
const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

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

/// What warm-up says about a delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// Send from this address.
    Use(IpAddress),
    /// No address of the pool has room for this provider today.
    Wait,
}

#[derive(Default)]
struct Active {
    /// Warming address id -> its profile.
    profiles: HashMap<Id, WarmupProfile>,
    loaded_at: Option<Instant>,
}

pub struct WarmupGate {
    pub thresholds: WarmupThresholds,
    /// How long a message waits when no address has room (`warmup.defer_seconds`).
    pub defer_seconds: i64,
    active: Mutex<Active>,
}

impl WarmupGate {
    pub fn new(config: &camelmailer_config::Warmup) -> Self {
        Self {
            thresholds: thresholds_from(config),
            defer_seconds: i64::from(config.defer_seconds),
            active: Mutex::new(Active::default()),
        }
    }

    /// Re-read the warming addresses when the last read is older than a
    /// minute. A failed read keeps the previous set.
    pub async fn refresh(&self, store: &dyn WarmupStore) -> Result<(), StoreError> {
        let due = self
            .active
            .lock()
            .unwrap()
            .loaded_at
            .is_none_or(|at| at.elapsed() >= REFRESH_INTERVAL);
        if !due {
            return Ok(());
        }
        self.reload(store).await
    }

    /// Re-read the warming addresses now.
    pub async fn reload(&self, store: &dyn WarmupStore) -> Result<(), StoreError> {
        let addresses = store.warmup_addresses().await;
        let mut active = self.active.lock().unwrap();
        active.loaded_at = Some(Instant::now());
        let addresses = addresses?;
        active.profiles = addresses
            .iter()
            .filter_map(|a| a.effective_profile().map(|p| (a.ip_address_id, p)))
            .collect();
        Ok(())
    }

    /// The profile of a warming address.
    pub fn profile(&self, ip_address_id: Id) -> Option<WarmupProfile> {
        self.active
            .lock()
            .unwrap()
            .profiles
            .get(&ip_address_id)
            .copied()
    }

    /// Whether any of `candidates` warms up. False whenever warm-up is off
    /// everywhere, so the caller leaves source selection alone.
    pub fn covers(&self, candidates: &[IpAddress]) -> bool {
        let active = self.active.lock().unwrap();
        !active.profiles.is_empty()
            && candidates
                .iter()
                .any(|a| active.profiles.contains_key(&a.id))
    }

    /// Choose the address a message to `provider` goes out from: `chosen`
    /// (what source selection picked) if it has room, else the next of
    /// `candidates` (lowest priority number, then id) that has. An address
    /// that does not warm up always has room. A storage error lets the
    /// message go out from `chosen`, as without warm-up.
    pub async fn pick(
        &self,
        store: &dyn WarmupStore,
        chosen: &IpAddress,
        candidates: &[IpAddress],
        provider: ProviderGroup,
        now: DateTime<Utc>,
    ) -> Pick {
        let mut order: Vec<&IpAddress> = vec![chosen];
        let mut rest: Vec<&IpAddress> = candidates.iter().filter(|a| a.id != chosen.id).collect();
        rest.sort_by_key(|a| (a.priority, a.id));
        order.extend(rest);
        for address in order {
            let Some(profile) = self.profile(address.id) else {
                return Pick::Use(address.clone());
            };
            let initial = WarmupState::initial(address.id, provider, &profile);
            match store
                .try_reserve_warmup(address.id, provider, now.date_naive(), &initial, now)
                .await
            {
                Ok(true) => return Pick::Use(address.clone()),
                Ok(false) => continue,
                Err(error) => {
                    tracing::warn!(%error, ip_address_id = address.id, "warm-up reservation failed; sending without warm-up");
                    return Pick::Use(chosen.clone());
                }
            }
        }
        Pick::Wait
    }

    /// After a policy refusal: brake at once when today's refusals from the
    /// address to the provider reach the brake limits. Returns the reason
    /// when it braked.
    pub async fn brake_if_blocked(
        &self,
        store: &dyn WarmupStore,
        ip_address_id: Id,
        provider: ProviderGroup,
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
            .find(|s| s.ip_address_id == ip_address_id && s.provider == provider)
        else {
            return Ok(None);
        };
        let Some(braked) = camelmailer_core::warmup::intraday_brake(
            &state,
            &profile,
            today,
            &self.thresholds,
            now,
        ) else {
            return Ok(None);
        };
        let entry = WarmupLogEntry {
            ip_address_id,
            provider: Some(provider),
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
        signals: &(dyn Fn(Id, ProviderGroup) -> Vec<WarmupSignal> + Send + Sync),
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
            let ipv4 = addresses.get(&state.ip_address_id);
            let stat = stats.iter().find(|s| {
                s.day == day
                    && s.provider == state.provider
                    && (s.ip_address_id == Some(state.ip_address_id)
                        || ipv4.is_some_and(|ip| &s.source_ip == ip))
            });
            let found = signals(state.ip_address_id, state.provider);
            let (next, decision) = camelmailer_core::warmup::evaluate(
                &state,
                &profile,
                day,
                stat,
                &found,
                &self.thresholds,
                now,
            );
            let entry = WarmupLogEntry {
                ip_address_id: state.ip_address_id,
                provider: Some(state.provider),
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
        let custom = WarmupProfile {
            start: 2,
            growth: 2.0,
            cap: 100,
        };
        store
            .set_address_warmup(addresses[0].id, Some(WarmupMode::Custom), Some(custom))
            .await
            .unwrap();
        store
            .set_address_warmup(addresses[1].id, Some(WarmupMode::Custom), Some(custom))
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
                    ProviderGroup::Microsoft,
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
                ProviderGroup::Google,
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
                Some(WarmupProfile {
                    start: 1,
                    growth: 2.0,
                    cap: 100,
                }),
            )
            .await
            .unwrap();
        let gate = WarmupGate::new(&Default::default());
        gate.reload(&store).await.unwrap();
        let now = Utc::now();
        assert_eq!(
            gate.pick(&store, &addresses[0], &addresses, ProviderGroup::Yahoo, now)
                .await,
            Pick::Use(addresses[0].clone())
        );
        for _ in 0..3 {
            assert_eq!(
                gate.pick(&store, &addresses[0], &addresses, ProviderGroup::Yahoo, now)
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
        let initial = WarmupState::initial(
            addresses[0].id,
            ProviderGroup::Google,
            &WarmupProfile::NORMAL,
        );
        let morning = DateTime::parse_from_rfc3339("2026-11-14T08:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        assert!(store
            .try_reserve_warmup(
                addresses[0].id,
                ProviderGroup::Google,
                day,
                &initial,
                morning
            )
            .await
            .unwrap());
        let stats = vec![IpDeliveryStat {
            day,
            source_ip: addresses[0].ipv4.clone(),
            ip_address_id: Some(addresses[0].id),
            provider: ProviderGroup::Google,
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
        let none = |_: Id, _: ProviderGroup| Vec::new();
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
}
