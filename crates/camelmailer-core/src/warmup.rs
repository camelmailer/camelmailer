//! Adaptive IP warm-up.
//!
//! A new sending address has no reputation, and mailbox providers throttle
//! or refuse what they do not know. Warm-up gives each pool address a daily
//! allowance **per receiving provider group** (Microsoft, Google, Yahoo,
//! others; an address can be warm at one and still cold at another) and
//! moves it along by what the address actually achieved:
//!
//! * it **advances** (allowance × growth) only when the address used most of
//!   its allowance the day before and every signal was healthy;
//! * it **holds** when the volume was too low to judge;
//! * it **slows** (no growth) on rising temporary deferrals from that
//!   provider, a complaint rate above the warning level or a warning signal;
//! * it **brakes** (pause for a while and step the allowance back) on policy
//!   refusals and blocks, a high bounce rate, a complaint rate above the hard
//!   level or a bad signal. A run of policy refusals during the day brakes at
//!   once ([`intraday_brake`]).
//!
//! Warm-up is **off by default**. It applies only to an address whose own
//! mode, or its pool's, is `slow`, `normal`, `fast` or `custom`. With every
//! address off the worker never consults it, and delivery is exactly as
//! without it. Switching modes continues from the address's current
//! allowance; it never restarts the ramp.
//!
//! Volume beyond an address's allowance goes to another address of the same
//! pool that has room (an address with warm-up off counts as warm). Only
//! when none has room does the message wait, without an attempt counted.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::delivery_stats::{IpDeliveryStat, ProviderGroup};
use crate::{Id, StoreError};

/// How an address warms up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WarmupMode {
    /// No warm-up: the address sends without a limit (the default).
    Off,
    Slow,
    Normal,
    Fast,
    /// The address's (or pool's) own [`WarmupProfile`].
    Custom,
}

impl WarmupMode {
    pub fn as_str(self) -> &'static str {
        match self {
            WarmupMode::Off => "off",
            WarmupMode::Slow => "slow",
            WarmupMode::Normal => "normal",
            WarmupMode::Fast => "fast",
            WarmupMode::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Option<WarmupMode> {
        match value {
            "off" => Some(WarmupMode::Off),
            "slow" => Some(WarmupMode::Slow),
            "normal" => Some(WarmupMode::Normal),
            "fast" => Some(WarmupMode::Fast),
            "custom" => Some(WarmupMode::Custom),
            _ => None,
        }
    }
}

/// The ramp of one address with one provider group: the first day's
/// allowance, the factor it grows by after each healthy day, and the
/// allowance from which the address counts as warm (no limit any more).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ramp {
    pub start: i64,
    pub growth: f64,
    pub cap: i64,
}

impl Ramp {
    /// Why a ramp is not usable, if it is not.
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=1_000_000).contains(&self.start) {
            return Err("start must be between 1 and 1000000".into());
        }
        if !(1.05..=3.0).contains(&self.growth) {
            return Err("growth must be between 1.05 and 3.0".into());
        }
        if self.cap <= self.start || self.cap > 10_000_000 {
            return Err("cap must be above start and at most 10000000".into());
        }
        Ok(())
    }

    /// The allowance after one more healthy day.
    pub fn next(&self, allowance: i64) -> i64 {
        let grown = (allowance as f64 * self.growth).ceil() as i64;
        grown.max(allowance + 1).min(self.cap)
    }

    /// Healthy days still needed from `allowance` to `cap`.
    pub fn days_to_warm(&self, allowance: i64) -> u32 {
        let mut days = 0;
        let mut current = allowance.max(1);
        while current < self.cap && days < 365 {
            current = self.next(current);
            days += 1;
        }
        days
    }
}

/// The first day's allowance and the cap of one provider group in a
/// profile; the growth is the profile's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupLimits {
    pub start: i64,
    pub cap: i64,
}

/// A warm-up profile: a ramp for every provider group. `groups` sets the
/// first day's allowance and the cap per group; a group it does not name
/// (`other`, and any group added in the config) uses `start` and `cap`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WarmupProfile {
    pub start: i64,
    pub growth: f64,
    pub cap: i64,
    #[serde(default)]
    pub groups: BTreeMap<String, GroupLimits>,
}

impl WarmupProfile {
    /// A profile without per-group limits.
    pub fn uniform(start: i64, growth: f64, cap: i64) -> Self {
        WarmupProfile {
            start,
            growth,
            cap,
            groups: BTreeMap::new(),
        }
    }

    fn with_groups(start: i64, growth: f64, cap: i64, groups: &[(&str, i64, i64)]) -> Self {
        WarmupProfile {
            start,
            growth,
            cap,
            groups: groups
                .iter()
                .map(|(name, start, cap)| {
                    (
                        name.to_string(),
                        GroupLimits {
                            start: *start,
                            cap: *cap,
                        },
                    )
                })
                .collect(),
        }
    }

    /// The built-in `slow` profile. Microsoft, Apple and T-Online are the
    /// most cautious with unknown senders, so they start lower.
    pub fn slow() -> Self {
        Self::with_groups(
            50,
            1.3,
            20_000,
            &[
                ("microsoft", 30, 15_000),
                ("google", 50, 20_000),
                ("yahoo", 40, 15_000),
                ("apple", 30, 10_000),
                ("united_internet", 30, 12_000),
                ("t_online", 20, 8_000),
            ],
        )
    }

    /// The built-in `normal` profile.
    pub fn normal() -> Self {
        Self::with_groups(
            200,
            1.5,
            50_000,
            &[
                ("microsoft", 100, 40_000),
                ("google", 200, 50_000),
                ("yahoo", 150, 40_000),
                ("apple", 100, 20_000),
                ("united_internet", 100, 30_000),
                ("t_online", 50, 15_000),
            ],
        )
    }

    /// The built-in `fast` profile.
    pub fn fast() -> Self {
        Self::with_groups(
            500,
            2.0,
            100_000,
            &[
                ("microsoft", 300, 80_000),
                ("google", 500, 100_000),
                ("yahoo", 400, 80_000),
                ("apple", 250, 40_000),
                ("united_internet", 250, 60_000),
                ("t_online", 100, 30_000),
            ],
        )
    }

    /// The ramp of `group` in this profile.
    pub fn ramp(&self, group: &ProviderGroup) -> Ramp {
        match self.groups.get(group.as_str()) {
            Some(limits) => Ramp {
                start: limits.start,
                growth: self.growth,
                cap: limits.cap,
            },
            None => Ramp {
                start: self.start,
                growth: self.growth,
                cap: self.cap,
            },
        }
    }

    /// Why a profile is not usable, if it is not.
    pub fn validate(&self) -> Result<(), String> {
        Ramp {
            start: self.start,
            growth: self.growth,
            cap: self.cap,
        }
        .validate()?;
        for (name, limits) in &self.groups {
            if ProviderGroup::parse(name).is_none() {
                return Err(format!("groups: {name:?} is not a valid group name"));
            }
            Ramp {
                start: limits.start,
                growth: self.growth,
                cap: limits.cap,
            }
            .validate()
            .map_err(|e| format!("groups.{name}: {e}"))?;
        }
        Ok(())
    }
}

/// The built-in profiles as the installation has configured them
/// (`warmup.profiles`).
#[derive(Debug, Clone, PartialEq)]
pub struct WarmupProfiles {
    pub slow: WarmupProfile,
    pub normal: WarmupProfile,
    pub fast: WarmupProfile,
}

impl Default for WarmupProfiles {
    fn default() -> Self {
        WarmupProfiles {
            slow: WarmupProfile::slow(),
            normal: WarmupProfile::normal(),
            fast: WarmupProfile::fast(),
        }
    }
}

impl WarmupProfiles {
    /// The profile of `mode`; `custom` uses `custom`. `None` for `off`, and
    /// for `custom` without a usable profile.
    pub fn for_mode(
        &self,
        mode: WarmupMode,
        custom: Option<&WarmupProfile>,
    ) -> Option<WarmupProfile> {
        let profile = match mode {
            WarmupMode::Off => return None,
            WarmupMode::Slow => self.slow.clone(),
            WarmupMode::Normal => self.normal.clone(),
            WarmupMode::Fast => self.fast.clone(),
            WarmupMode::Custom => custom?.clone(),
        };
        profile.validate().is_ok().then_some(profile)
    }

    /// Override a built-in profile's base values and per-group limits (the
    /// `warmup.profiles` config). Unknown profile names are ignored.
    pub fn override_profile(
        &mut self,
        name: &str,
        start: Option<i64>,
        growth: Option<f64>,
        cap: Option<i64>,
        groups: &[(String, i64, i64)],
    ) {
        let profile = match name {
            "slow" => &mut self.slow,
            "normal" => &mut self.normal,
            "fast" => &mut self.fast,
            _ => return,
        };
        if let Some(start) = start {
            profile.start = start;
        }
        if let Some(growth) = growth {
            profile.growth = growth;
        }
        if let Some(cap) = cap {
            profile.cap = cap;
        }
        for (group, start, cap) in groups {
            profile.groups.insert(
                group.clone(),
                GroupLimits {
                    start: *start,
                    cap: *cap,
                },
            );
        }
    }

    /// Every profile is usable.
    pub fn validate(&self) -> Result<(), String> {
        for (name, profile) in [
            ("slow", &self.slow),
            ("normal", &self.normal),
            ("fast", &self.fast),
        ] {
            profile
                .validate()
                .map_err(|e| format!("warmup.profiles.{name}: {e}"))?;
        }
        Ok(())
    }
}

/// Where an address's warm-up stands with one provider group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WarmupStatus {
    /// Growing after each healthy day.
    Ramping,
    /// Not growing: too little volume to judge, or waiting after a pause.
    Holding,
    /// Not growing because of rising deferrals or warning signals.
    Slowed,
    /// Braked: sends nothing to this provider until `paused_until`.
    Paused,
    /// Reached the cap: no limit any more.
    Warm,
}

impl WarmupStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            WarmupStatus::Ramping => "ramping",
            WarmupStatus::Holding => "holding",
            WarmupStatus::Slowed => "slowed",
            WarmupStatus::Paused => "paused",
            WarmupStatus::Warm => "warm",
        }
    }

    pub fn parse(value: &str) -> Option<WarmupStatus> {
        match value {
            "ramping" => Some(WarmupStatus::Ramping),
            "holding" => Some(WarmupStatus::Holding),
            "slowed" => Some(WarmupStatus::Slowed),
            "paused" => Some(WarmupStatus::Paused),
            "warm" => Some(WarmupStatus::Warm),
            _ => None,
        }
    }
}

/// The warm-up state of one address with one provider group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WarmupState {
    pub ip_address_id: Id,
    pub provider: ProviderGroup,
    /// Attempts allowed per UTC day.
    pub allowance: i64,
    pub status: WarmupStatus,
    pub paused_until: Option<DateTime<Utc>>,
    /// The last day whose counters were judged.
    pub evaluated_day: Option<NaiveDate>,
    /// Why the last decision was taken.
    pub last_reason: String,
    pub last_brake_reason: Option<String>,
    pub last_brake_at: Option<DateTime<Utc>>,
    /// Optimistic concurrency: every update must name the version it read.
    pub version: i64,
}

impl WarmupState {
    /// The state of an address that starts warming up on `ramp`.
    pub fn initial(ip_address_id: Id, provider: ProviderGroup, profile: &Ramp) -> Self {
        WarmupState {
            ip_address_id,
            provider,
            allowance: profile.start,
            status: WarmupStatus::Ramping,
            paused_until: None,
            evaluated_day: None,
            last_reason: "warm-up started".into(),
            last_brake_reason: None,
            last_brake_at: None,
            version: 0,
        }
    }

    /// Whether the address may not send to this provider at `now`.
    pub fn paused_at(&self, now: DateTime<Utc>) -> bool {
        self.status == WarmupStatus::Paused && self.paused_until.is_some_and(|until| until > now)
    }
}

/// A verdict from outside the delivery counters about an address and a
/// provider (Postmaster Tools reputation, SNDS, feedback loops, seed
/// placement): `Ok` allows the next step, `Warn` slows, `Bad` brakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalVerdict {
    Ok,
    Warn,
    Bad,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WarmupSignal {
    pub source: String,
    pub verdict: SignalVerdict,
    pub detail: String,
}

/// The limits warm-up judges by (config `warmup`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WarmupThresholds {
    /// Below this share of the allowance the day is too quiet to judge.
    pub min_volume_fraction: f64,
    /// The share of the allowance an address must have used to advance.
    pub advance_fraction: f64,
    /// Deferral rate (of attempts) from which the ramp slows.
    pub deferral_rate_slow: f64,
    /// Bounce rate (of attempts) from which the ramp brakes.
    pub bounce_rate_brake: f64,
    /// Policy refusals that brake (both the count and the rate must be
    /// reached).
    pub policy_blocks_brake: i64,
    pub policy_block_rate_brake: f64,
    /// Complaint rate (of accepted messages) that slows / brakes. Google
    /// asks senders to stay below 0.1 % and never reach 0.3 %.
    pub complaint_rate_warn: f64,
    pub complaint_rate_brake: f64,
    /// How long a brake pauses the address for that provider.
    pub pause_hours: i64,
    /// The factor a brake steps the allowance back by.
    pub step_back: f64,
}

impl Default for WarmupThresholds {
    fn default() -> Self {
        WarmupThresholds {
            min_volume_fraction: 0.5,
            advance_fraction: 0.9,
            deferral_rate_slow: 0.05,
            bounce_rate_brake: 0.05,
            policy_blocks_brake: 3,
            policy_block_rate_brake: 0.01,
            complaint_rate_warn: 0.001,
            complaint_rate_brake: 0.003,
            pause_hours: 24,
            step_back: 0.5,
        }
    }
}

/// What a daily evaluation decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WarmupDecision {
    Advance,
    Hold,
    Slow,
    Brake,
    Warm,
}

impl WarmupDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            WarmupDecision::Advance => "advance",
            WarmupDecision::Hold => "hold",
            WarmupDecision::Slow => "slow",
            WarmupDecision::Brake => "brake",
            WarmupDecision::Warm => "warm",
        }
    }
}

/// One logged warm-up decision (`ip_warmup_log`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WarmupLogEntry {
    pub ip_address_id: Id,
    pub provider: Option<ProviderGroup>,
    /// `advance`, `hold`, `slow`, `brake`, `warm` or `mode`.
    pub decision: String,
    pub reason: String,
    pub allowance_before: Option<i64>,
    pub allowance_after: Option<i64>,
    pub created_at: DateTime<Utc>,
}

fn rate(part: i64, whole: i64) -> f64 {
    if whole <= 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

fn percent(value: f64) -> String {
    format!("{:.2} %", value * 100.0)
}

/// Brake `state`: pause until `now + pause_hours` and step the allowance
/// back (never below the profile's start).
fn brake(
    state: &WarmupState,
    profile: &Ramp,
    thresholds: &WarmupThresholds,
    now: DateTime<Utc>,
    reason: String,
) -> WarmupState {
    let stepped = ((state.allowance as f64) * thresholds.step_back).floor() as i64;
    WarmupState {
        allowance: stepped.max(profile.start).min(state.allowance),
        status: WarmupStatus::Paused,
        paused_until: Some(now + chrono::Duration::hours(thresholds.pause_hours)),
        last_reason: reason.clone(),
        last_brake_reason: Some(reason),
        last_brake_at: Some(now),
        ..state.clone()
    }
}

/// Judge one finished day of an address with one provider and return the
/// new state with the decision. `stat` is that day's counters (`None` when
/// nothing was sent); `signals` are outside verdicts for the address and
/// provider. The returned state has `evaluated_day = day`; the caller keeps
/// `version` for its update.
pub fn evaluate(
    state: &WarmupState,
    profile: &Ramp,
    day: NaiveDate,
    stat: Option<&IpDeliveryStat>,
    signals: &[WarmupSignal],
    thresholds: &WarmupThresholds,
    now: DateTime<Utc>,
) -> (WarmupState, WarmupDecision) {
    let (attempts, sent, deferred, bounced, blocks, complaints) = match stat {
        Some(s) => (
            s.attempts(),
            s.sent,
            s.deferred,
            s.bounced,
            s.policy_blocks,
            s.complaints,
        ),
        None => (0, 0, 0, 0, 0, 0),
    };
    let mut judged = state.clone();
    judged.evaluated_day = Some(day);

    // Hard brakes first, whatever the volume.
    let bad: Vec<&WarmupSignal> = signals
        .iter()
        .filter(|s| s.verdict == SignalVerdict::Bad)
        .collect();
    let mut brake_reasons = Vec::new();
    if blocks >= thresholds.policy_blocks_brake
        && rate(blocks, attempts) >= thresholds.policy_block_rate_brake
    {
        brake_reasons.push(format!(
            "{blocks} policy refusals ({}) on {day}",
            percent(rate(blocks, attempts))
        ));
    }
    if attempts > 0 && rate(bounced, attempts) >= thresholds.bounce_rate_brake && bounced >= 3 {
        brake_reasons.push(format!(
            "bounce rate {} on {day}",
            percent(rate(bounced, attempts))
        ));
    }
    if complaints > 0 && rate(complaints, sent) >= thresholds.complaint_rate_brake {
        brake_reasons.push(format!(
            "complaint rate {} on {day} (hard limit {})",
            percent(rate(complaints, sent)),
            percent(thresholds.complaint_rate_brake)
        ));
    }
    for signal in &bad {
        brake_reasons.push(format!("{}: {}", signal.source, signal.detail));
    }
    if !brake_reasons.is_empty() {
        let reason = format!("brake: {}", brake_reasons.join("; "));
        let mut braked = brake(&judged, profile, thresholds, now, reason);
        braked.evaluated_day = Some(day);
        return (braked, WarmupDecision::Brake);
    }

    // A pause that has run out ends in holding; the next healthy day grows.
    if state.status == WarmupStatus::Paused && !state.paused_at(now) {
        judged.status = WarmupStatus::Holding;
        judged.paused_until = None;
        judged.last_reason = "pause over; holding for one day before growing again".into();
        return (judged, WarmupDecision::Hold);
    }
    if state.paused_at(now) {
        judged.last_reason = format!(
            "paused until {}",
            state
                .paused_until
                .map(|until| until.to_rfc3339())
                .unwrap_or_default()
        );
        return (judged, WarmupDecision::Hold);
    }

    if state.allowance >= profile.cap {
        judged.status = WarmupStatus::Warm;
        judged.allowance = state.allowance.max(profile.cap);
        judged.last_reason = format!("warm: allowance reached the cap of {}", profile.cap);
        return (judged, WarmupDecision::Warm);
    }

    // Slow down on warnings.
    let warn: Vec<&WarmupSignal> = signals
        .iter()
        .filter(|s| s.verdict == SignalVerdict::Warn)
        .collect();
    let mut slow_reasons = Vec::new();
    if attempts > 0 && rate(deferred, attempts) >= thresholds.deferral_rate_slow {
        slow_reasons.push(format!(
            "deferral rate {} on {day}",
            percent(rate(deferred, attempts))
        ));
    }
    if complaints > 0 && rate(complaints, sent) >= thresholds.complaint_rate_warn {
        slow_reasons.push(format!(
            "complaint rate {} on {day} (warning level {})",
            percent(rate(complaints, sent)),
            percent(thresholds.complaint_rate_warn)
        ));
    }
    for signal in &warn {
        slow_reasons.push(format!("{}: {}", signal.source, signal.detail));
    }
    if !slow_reasons.is_empty() {
        judged.status = WarmupStatus::Slowed;
        judged.last_reason = format!("slow: {}", slow_reasons.join("; "));
        return (judged, WarmupDecision::Slow);
    }

    // Too quiet to tell anything.
    let used = rate(attempts, state.allowance);
    if used < thresholds.min_volume_fraction {
        judged.status = WarmupStatus::Holding;
        judged.last_reason = format!(
            "hold: {attempts} of {} attempts on {day}, too little volume to judge",
            state.allowance
        );
        return (judged, WarmupDecision::Hold);
    }
    if used < thresholds.advance_fraction {
        judged.status = WarmupStatus::Holding;
        judged.last_reason = format!(
            "hold: {attempts} of {} attempts on {day}, below the {} needed to grow",
            state.allowance,
            percent(thresholds.advance_fraction)
        );
        return (judged, WarmupDecision::Hold);
    }

    let next = profile.next(state.allowance);
    judged.allowance = next;
    let signal_note = if signals.is_empty() {
        String::new()
    } else {
        format!(
            "; signals ok ({})",
            signals
                .iter()
                .map(|s| s.source.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    if next >= profile.cap {
        judged.status = WarmupStatus::Warm;
        judged.last_reason = format!(
            "warm: {attempts} healthy attempts on {day}, allowance reached the cap of {}{signal_note}",
            profile.cap
        );
        (judged, WarmupDecision::Warm)
    } else {
        judged.status = WarmupStatus::Ramping;
        judged.last_reason = format!(
            "advance: {attempts} healthy attempts of {} on {day}, allowance {} -> {next}{signal_note}",
            state.allowance, state.allowance
        );
        (judged, WarmupDecision::Advance)
    }
}

/// Brake at once during the day when policy refusals pile up (`today` is
/// the address's counters for the provider so far). `None` while they stay
/// below the brake limits, or when the address is already paused.
pub fn intraday_brake(
    state: &WarmupState,
    profile: &Ramp,
    today: &IpDeliveryStat,
    thresholds: &WarmupThresholds,
    now: DateTime<Utc>,
) -> Option<WarmupState> {
    if state.paused_at(now) {
        return None;
    }
    let attempts = today.attempts();
    let blocks = today.policy_blocks;
    if blocks >= thresholds.policy_blocks_brake
        && rate(blocks, attempts) >= thresholds.policy_block_rate_brake
    {
        let reason = format!(
            "brake: {blocks} policy refusals ({}) so far today",
            percent(rate(blocks, attempts))
        );
        return Some(brake(state, profile, thresholds, now, reason));
    }
    None
}

/// The warm-up settings of one pool address: its own mode and profile and
/// its pool's (`None` = not set).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WarmupAddress {
    pub ip_address_id: Id,
    pub ipv4: String,
    pub hostname: String,
    pub ip_pool_id: Id,
    pub pool_name: String,
    pub address_mode: Option<WarmupMode>,
    pub address_profile: Option<WarmupProfile>,
    pub pool_mode: Option<WarmupMode>,
    pub pool_profile: Option<WarmupProfile>,
}

impl WarmupAddress {
    /// The mode in force and where it comes from (`address`, `pool` or
    /// `default`).
    pub fn effective_mode(&self) -> (WarmupMode, &'static str) {
        match (self.address_mode, self.pool_mode) {
            (Some(mode), _) => (mode, "address"),
            (None, Some(mode)) => (mode, "pool"),
            (None, None) => (WarmupMode::Off, "default"),
        }
    }

    /// The profile in force, from the installation's `profiles`; `None`
    /// when warm-up is off (or `custom` without a usable profile, which
    /// counts as off).
    pub fn effective_profile(&self, profiles: &WarmupProfiles) -> Option<WarmupProfile> {
        let (mode, source) = self.effective_mode();
        let custom = match source {
            "address" => self.address_profile.as_ref().or(self.pool_profile.as_ref()),
            _ => self.pool_profile.as_ref(),
        };
        profiles.for_mode(mode, custom)
    }
}

/// Storage of the warm-up settings, state, daily usage and log.
#[async_trait]
pub trait WarmupStore: Send + Sync {
    /// Every pool address with its own and its pool's warm-up settings.
    async fn warmup_addresses(&self) -> Result<Vec<WarmupAddress>, StoreError>;
    /// Set an address's mode (`None` inherits the pool's) and profile.
    /// Returns false when the address does not exist.
    async fn set_address_warmup(
        &self,
        ip_address_id: Id,
        mode: Option<WarmupMode>,
        profile: Option<WarmupProfile>,
    ) -> Result<bool, StoreError>;
    /// Set a pool's default mode (`None` = off unless an address sets one).
    /// Returns false when the pool does not exist.
    async fn set_pool_warmup(
        &self,
        ip_pool_id: Id,
        mode: Option<WarmupMode>,
        profile: Option<WarmupProfile>,
    ) -> Result<bool, StoreError>;
    /// Every stored state.
    async fn warmup_states(&self) -> Result<Vec<WarmupState>, StoreError>;
    /// Replace a state if it is still at `state.version` (insert when
    /// `state.version` is 0 and none exists), bump the version and log
    /// `entry`. Returns false when another writer got there first.
    async fn save_warmup_state(
        &self,
        state: &WarmupState,
        entry: Option<&WarmupLogEntry>,
    ) -> Result<bool, StoreError>;
    /// Count one attempt from the address to the provider on `day` if it
    /// has room: not paused at `now`, and either warm or below its
    /// allowance. A missing state starts at `initial`. Returns whether the
    /// attempt was allowed (and counted).
    async fn try_reserve_warmup(
        &self,
        ip_address_id: Id,
        provider: ProviderGroup,
        day: NaiveDate,
        initial: &WarmupState,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError>;
    /// Attempts counted on `day`: (address, provider, used).
    async fn warmup_usage(
        &self,
        day: NaiveDate,
    ) -> Result<Vec<(Id, ProviderGroup, i64)>, StoreError>;
    /// Append to the log.
    async fn log_warmup(&self, entry: &WarmupLogEntry) -> Result<(), StoreError>;
    /// The newest log entries, of one address or all.
    async fn warmup_log(
        &self,
        ip_address_id: Option<Id>,
        limit: i64,
    ) -> Result<Vec<WarmupLogEntry>, StoreError>;
    /// Remove usage before `usage_before` and log entries before
    /// `log_before`.
    async fn prune_warmup(
        &self,
        usage_before: NaiveDate,
        log_before: DateTime<Utc>,
    ) -> Result<u64, StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    const NORMAL: Ramp = Ramp {
        start: 200,
        growth: 1.5,
        cap: 50_000,
    };

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-15T01:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 14).unwrap()
    }

    fn state(allowance: i64) -> WarmupState {
        WarmupState {
            allowance,
            ..WarmupState::initial(1, ProviderGroup::microsoft(), &NORMAL)
        }
    }

    fn stat(sent: i64, deferred: i64, bounced: i64) -> IpDeliveryStat {
        IpDeliveryStat {
            day: day(),
            source_ip: "203.0.113.5".into(),
            ip_address_id: Some(1),
            provider: ProviderGroup::microsoft(),
            sent,
            deferred,
            bounced,
            policy_blocks: 0,
            complaints: 0,
        }
    }

    fn judge(
        state: &WarmupState,
        stat: Option<&IpDeliveryStat>,
        signals: &[WarmupSignal],
    ) -> (WarmupState, WarmupDecision) {
        evaluate(
            state,
            &NORMAL,
            day(),
            stat,
            signals,
            &WarmupThresholds::default(),
            now(),
        )
    }

    #[test]
    fn a_healthy_full_day_advances_by_the_growth_factor() {
        let (next, decision) = judge(&state(200), Some(&stat(195, 4, 1)), &[]);
        assert_eq!(decision, WarmupDecision::Advance);
        assert_eq!(next.allowance, 300);
        assert_eq!(next.status, WarmupStatus::Ramping);
        assert_eq!(next.evaluated_day, Some(day()));
        assert!(
            next.last_reason.contains("200 -> 300"),
            "{}",
            next.last_reason
        );
    }

    #[test]
    fn a_quiet_day_holds() {
        let (next, decision) = judge(&state(200), Some(&stat(40, 0, 0)), &[]);
        assert_eq!(decision, WarmupDecision::Hold);
        assert_eq!(next.allowance, 200);
        assert!(next.last_reason.contains("too little volume"));
        let (next, decision) = judge(&state(200), None, &[]);
        assert_eq!(decision, WarmupDecision::Hold);
        assert_eq!(next.allowance, 200);
        let (next, decision) = judge(&state(200), Some(&stat(150, 0, 0)), &[]);
        assert_eq!(
            decision,
            WarmupDecision::Hold,
            "75 % used: not enough to grow"
        );
        assert_eq!(next.allowance, 200);
    }

    #[test]
    fn rising_deferrals_slow_the_ramp() {
        let (next, decision) = judge(&state(200), Some(&stat(180, 20, 0)), &[]);
        assert_eq!(decision, WarmupDecision::Slow);
        assert_eq!(next.allowance, 200);
        assert_eq!(next.status, WarmupStatus::Slowed);
        assert!(next.last_reason.contains("deferral rate 10.00 %"));
    }

    #[test]
    fn policy_refusals_brake_pause_and_step_back() {
        let mut blocked = stat(190, 5, 5);
        blocked.policy_blocks = 5;
        let (next, decision) = judge(&state(1000), Some(&blocked), &[]);
        assert_eq!(decision, WarmupDecision::Brake);
        assert_eq!(next.allowance, 500);
        assert_eq!(next.status, WarmupStatus::Paused);
        assert_eq!(next.paused_until, Some(now() + chrono::Duration::hours(24)));
        assert!(next
            .last_brake_reason
            .as_deref()
            .unwrap()
            .contains("5 policy refusals"));
        // Never below the start of the profile.
        let (next, _) = judge(&state(250), Some(&blocked), &[]);
        assert_eq!(next.allowance, 200);
    }

    #[test]
    fn complaints_slow_at_the_warning_level_and_brake_at_the_hard_level() {
        let mut warn = stat(2000, 0, 0);
        warn.complaints = 3; // 0.15 %
        let (_, decision) = judge(&state(2000), Some(&warn), &[]);
        assert_eq!(decision, WarmupDecision::Slow);
        let mut hard = stat(2000, 0, 0);
        hard.complaints = 7; // 0.35 %
        let (next, decision) = judge(&state(2000), Some(&hard), &[]);
        assert_eq!(decision, WarmupDecision::Brake);
        assert!(next.last_reason.contains("complaint rate 0.35 %"));
    }

    #[test]
    fn signals_slow_brake_or_let_the_ramp_advance() {
        let warn = WarmupSignal {
            source: "snds".into(),
            verdict: SignalVerdict::Warn,
            detail: "filter result YELLOW".into(),
        };
        let bad = WarmupSignal {
            source: "postmaster".into(),
            verdict: SignalVerdict::Bad,
            detail: "IP reputation BAD".into(),
        };
        let ok = WarmupSignal {
            source: "seed".into(),
            verdict: SignalVerdict::Ok,
            detail: "inbox".into(),
        };
        let full = stat(200, 0, 0);
        assert_eq!(
            judge(&state(200), Some(&full), std::slice::from_ref(&warn)).1,
            WarmupDecision::Slow
        );
        assert_eq!(
            judge(&state(200), None, std::slice::from_ref(&bad)).1,
            WarmupDecision::Brake
        );
        let (next, decision) = judge(&state(200), Some(&full), &[ok]);
        assert_eq!(decision, WarmupDecision::Advance);
        assert!(next.last_reason.contains("signals ok (seed)"));
    }

    #[test]
    fn a_pause_ends_in_holding_and_then_grows_again() {
        let mut blocked = stat(190, 5, 5);
        blocked.policy_blocks = 5;
        let (paused, _) = judge(&state(1000), Some(&blocked), &[]);
        // Still paused during the pause.
        let (still, decision) = judge(&paused, None, &[]);
        assert_eq!(decision, WarmupDecision::Hold);
        assert_eq!(still.status, WarmupStatus::Paused);
        // After the pause.
        let later = now() + chrono::Duration::hours(25);
        let (held, decision) = evaluate(
            &paused,
            &NORMAL,
            day(),
            None,
            &[],
            &WarmupThresholds::default(),
            later,
        );
        assert_eq!(decision, WarmupDecision::Hold);
        assert_eq!(held.status, WarmupStatus::Holding);
        assert_eq!(held.paused_until, None);
        let (grown, decision) = evaluate(
            &held,
            &NORMAL,
            day(),
            Some(&stat(500, 0, 0)),
            &[],
            &WarmupThresholds::default(),
            later,
        );
        assert_eq!(decision, WarmupDecision::Advance);
        assert_eq!(grown.allowance, 750);
    }

    #[test]
    fn the_cap_makes_an_address_warm() {
        let (next, decision) = judge(&state(40_000), Some(&stat(39_000, 0, 0)), &[]);
        assert_eq!(decision, WarmupDecision::Warm);
        assert_eq!(next.allowance, 50_000);
        assert_eq!(next.status, WarmupStatus::Warm);
    }

    #[test]
    fn an_intraday_run_of_refusals_brakes_at_once() {
        let mut today = stat(50, 0, 2);
        today.policy_blocks = 2;
        assert!(intraday_brake(
            &state(400),
            &NORMAL,
            &today,
            &WarmupThresholds::default(),
            now()
        )
        .is_none());
        today.policy_blocks = 3;
        today.bounced = 3;
        let braked = intraday_brake(
            &state(400),
            &NORMAL,
            &today,
            &WarmupThresholds::default(),
            now(),
        )
        .unwrap();
        assert_eq!(braked.status, WarmupStatus::Paused);
        assert_eq!(braked.allowance, 200);
        assert!(
            intraday_brake(
                &braked,
                &NORMAL,
                &today,
                &WarmupThresholds::default(),
                now()
            )
            .is_none(),
            "already paused"
        );
    }

    #[test]
    fn profiles_and_estimates() {
        assert_eq!(NORMAL.days_to_warm(200), 14);
        let fast = WarmupProfile::fast().ramp(&ProviderGroup::other());
        assert_eq!(fast.days_to_warm(500), 8);
        assert_eq!(NORMAL.days_to_warm(50_000), 0);
        assert!(Ramp {
            start: 10,
            growth: 1.0,
            cap: 100
        }
        .validate()
        .is_err());
        assert!(Ramp {
            start: 10,
            growth: 1.2,
            cap: 10
        }
        .validate()
        .is_err());
        assert!(Ramp {
            start: 10,
            growth: 1.2,
            cap: 1000
        }
        .validate()
        .is_ok());
        let profiles = WarmupProfiles::default();
        assert!(profiles.validate().is_ok());
        assert_eq!(profiles.for_mode(WarmupMode::Off, None), None);
        assert_eq!(profiles.for_mode(WarmupMode::Custom, None), None);
    }

    #[test]
    fn every_group_has_its_own_allowance_in_every_profile() {
        let profiles = WarmupProfiles::default();
        for profile in [&profiles.slow, &profiles.normal, &profiles.fast] {
            for group in [
                "microsoft",
                "google",
                "yahoo",
                "apple",
                "united_internet",
                "t_online",
            ] {
                assert!(profile.groups.contains_key(group), "{group}");
            }
        }
        let normal = &profiles.normal;
        assert_eq!(normal.ramp(&ProviderGroup::microsoft()).start, 100);
        assert_eq!(normal.ramp(&ProviderGroup::t_online()).cap, 15_000);
        // A group added in the config uses the profile's base values.
        let seznam = ProviderGroup::parse("seznam").unwrap();
        assert_eq!(normal.ramp(&seznam), NORMAL);
        assert_eq!(normal.ramp(&ProviderGroup::other()), NORMAL);

        let mut configured = WarmupProfiles::default();
        configured.override_profile(
            "normal",
            None,
            Some(1.4),
            None,
            &[("seznam".into(), 80, 25_000)],
        );
        configured.override_profile("turbo", Some(1), None, None, &[]);
        let ramp = configured.normal.ramp(&seznam);
        assert_eq!((ramp.start, ramp.growth, ramp.cap), (80, 1.4, 25_000));
        assert_eq!(configured.normal.ramp(&ProviderGroup::google()).growth, 1.4);
        assert!(configured.validate().is_ok());
        configured.override_profile("slow", None, None, None, &[("bad".into(), 100, 50)]);
        assert!(configured.validate().is_err());
    }

    #[test]
    fn custom_profiles_may_set_group_limits() {
        let custom: WarmupProfile = serde_json::from_value(serde_json::json!({
            "start": 10, "growth": 2.0, "cap": 1000,
            "groups": { "microsoft": { "start": 5, "cap": 500 } }
        }))
        .unwrap();
        assert!(custom.validate().is_ok());
        assert_eq!(custom.ramp(&ProviderGroup::microsoft()).start, 5);
        assert_eq!(custom.ramp(&ProviderGroup::google()).start, 10);
        let bad: WarmupProfile = serde_json::from_value(serde_json::json!({
            "start": 10, "growth": 2.0, "cap": 1000,
            "groups": { "Microsoft": { "start": 5, "cap": 500 } }
        }))
        .unwrap();
        assert!(bad.validate().is_err());
    }

    #[test]
    fn the_address_mode_wins_over_the_pool_mode() {
        let mut address = WarmupAddress {
            ip_address_id: 1,
            ipv4: "203.0.113.5".into(),
            hostname: "out1.example.com".into(),
            ip_pool_id: 1,
            pool_name: "Main".into(),
            address_mode: None,
            address_profile: None,
            pool_mode: None,
            pool_profile: None,
        };
        assert_eq!(address.effective_mode(), (WarmupMode::Off, "default"));
        assert_eq!(address.effective_profile(&WarmupProfiles::default()), None);
        address.pool_mode = Some(WarmupMode::Slow);
        assert_eq!(address.effective_mode(), (WarmupMode::Slow, "pool"));
        address.address_mode = Some(WarmupMode::Off);
        assert_eq!(address.effective_mode(), (WarmupMode::Off, "address"));
        assert_eq!(address.effective_profile(&WarmupProfiles::default()), None);
        address.address_mode = Some(WarmupMode::Custom);
        address.address_profile = Some(WarmupProfile::uniform(10, 1.2, 1000));
        assert_eq!(
            address
                .effective_profile(&WarmupProfiles::default())
                .unwrap()
                .start,
            10
        );
    }
}

#[async_trait]
impl WarmupStore for crate::MemoryStore {
    async fn warmup_addresses(&self) -> Result<Vec<WarmupAddress>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut addresses: Vec<WarmupAddress> = inner
            .ip_addresses
            .values()
            .filter_map(|address| {
                let pool = inner.ip_pools.get(&address.ip_pool_id)?;
                let own = inner.warmup_address_settings.get(&address.id);
                let pooled = inner.warmup_pool_settings.get(&pool.id);
                Some(WarmupAddress {
                    ip_address_id: address.id,
                    ipv4: address.ipv4.clone(),
                    hostname: address.hostname.clone(),
                    ip_pool_id: pool.id,
                    pool_name: pool.name.clone(),
                    address_mode: own.map(|(mode, _)| *mode),
                    address_profile: own.and_then(|(_, profile)| profile.clone()),
                    pool_mode: pooled.map(|(mode, _)| *mode),
                    pool_profile: pooled.and_then(|(_, profile)| profile.clone()),
                })
            })
            .collect();
        addresses.sort_by_key(|a| (a.ip_pool_id, a.ip_address_id));
        Ok(addresses)
    }

    async fn set_address_warmup(
        &self,
        ip_address_id: Id,
        mode: Option<WarmupMode>,
        profile: Option<WarmupProfile>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        if !inner.ip_addresses.contains_key(&ip_address_id) {
            return Ok(false);
        }
        match mode {
            Some(mode) => {
                inner
                    .warmup_address_settings
                    .insert(ip_address_id, (mode, profile));
            }
            None => {
                inner.warmup_address_settings.remove(&ip_address_id);
            }
        }
        Ok(true)
    }

    async fn set_pool_warmup(
        &self,
        ip_pool_id: Id,
        mode: Option<WarmupMode>,
        profile: Option<WarmupProfile>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        if !inner.ip_pools.contains_key(&ip_pool_id) {
            return Ok(false);
        }
        match mode {
            Some(mode) => {
                inner
                    .warmup_pool_settings
                    .insert(ip_pool_id, (mode, profile));
            }
            None => {
                inner.warmup_pool_settings.remove(&ip_pool_id);
            }
        }
        Ok(true)
    }

    async fn warmup_states(&self) -> Result<Vec<WarmupState>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut states: Vec<WarmupState> = inner.warmup_states.values().cloned().collect();
        states.sort_by_key(|s| (s.ip_address_id, s.provider.clone()));
        Ok(states)
    }

    async fn save_warmup_state(
        &self,
        state: &WarmupState,
        entry: Option<&WarmupLogEntry>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let key = (state.ip_address_id, state.provider.clone());
        let current = inner.warmup_states.get(&key).map(|s| s.version);
        let matches = match current {
            Some(version) => version == state.version,
            None => state.version == 0,
        };
        if !matches {
            return Ok(false);
        }
        let mut saved = state.clone();
        saved.version = state.version + 1;
        inner.warmup_states.insert(key, saved);
        if let Some(entry) = entry {
            inner.warmup_log.push(entry.clone());
        }
        Ok(true)
    }

    async fn try_reserve_warmup(
        &self,
        ip_address_id: Id,
        provider: ProviderGroup,
        day: NaiveDate,
        initial: &WarmupState,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let state = inner
            .warmup_states
            .entry((ip_address_id, provider.clone()))
            .or_insert_with(|| WarmupState {
                version: 1,
                ..initial.clone()
            })
            .clone();
        if state.paused_at(now) {
            return Ok(false);
        }
        let used = inner
            .warmup_usage
            .entry((day, ip_address_id, provider.clone()))
            .or_insert(0);
        if state.status != WarmupStatus::Warm && *used >= state.allowance {
            return Ok(false);
        }
        *used += 1;
        Ok(true)
    }

    async fn warmup_usage(
        &self,
        day: NaiveDate,
    ) -> Result<Vec<(Id, ProviderGroup, i64)>, StoreError> {
        let inner = self.inner.read().unwrap();
        let mut usage: Vec<(Id, ProviderGroup, i64)> = inner
            .warmup_usage
            .iter()
            .filter(|((d, _, _), _)| *d == day)
            .map(|((_, id, provider), used)| (*id, provider.clone(), *used))
            .collect();
        usage.sort();
        Ok(usage)
    }

    async fn log_warmup(&self, entry: &WarmupLogEntry) -> Result<(), StoreError> {
        self.inner.write().unwrap().warmup_log.push(entry.clone());
        Ok(())
    }

    async fn warmup_log(
        &self,
        ip_address_id: Option<Id>,
        limit: i64,
    ) -> Result<Vec<WarmupLogEntry>, StoreError> {
        let inner = self.inner.read().unwrap();
        Ok(inner
            .warmup_log
            .iter()
            .rev()
            .filter(|entry| ip_address_id.is_none_or(|id| entry.ip_address_id == id))
            .take(limit.max(0) as usize)
            .cloned()
            .collect())
    }

    async fn prune_warmup(
        &self,
        usage_before: NaiveDate,
        log_before: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        let mut inner = self.inner.write().unwrap();
        let before = inner.warmup_usage.len() + inner.warmup_log.len();
        inner
            .warmup_usage
            .retain(|(day, _, _), _| *day >= usage_before);
        inner
            .warmup_log
            .retain(|entry| entry.created_at >= log_before);
        Ok((before - inner.warmup_usage.len() - inner.warmup_log.len()) as u64)
    }
}

#[cfg(test)]
mod memory_tests {
    use super::*;
    use crate::AdminStore;

    #[tokio::test]
    async fn reservations_stop_at_the_allowance_and_while_paused() {
        let store = crate::MemoryStore::new();
        let pool = store.create_ip_pool("Main", true).await.unwrap();
        let address = store
            .create_ip_address(crate::NewIpAddress {
                ip_pool_id: pool.id,
                ipv4: "203.0.113.5".into(),
                ipv6: None,
                hostname: "out1.example.com".into(),
                priority: 100,
            })
            .await
            .unwrap();
        let profile = Ramp {
            start: 2,
            growth: 2.0,
            cap: 100,
        };
        let initial = WarmupState::initial(address.id, ProviderGroup::google(), &profile);
        let day = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let now = Utc::now();
        for expected in [true, true, false] {
            assert_eq!(
                store
                    .try_reserve_warmup(address.id, ProviderGroup::google(), day, &initial, now)
                    .await
                    .unwrap(),
                expected
            );
        }
        // Another provider has its own allowance.
        assert!(store
            .try_reserve_warmup(address.id, ProviderGroup::yahoo(), day, &initial, now)
            .await
            .unwrap());
        assert_eq!(
            store.warmup_usage(day).await.unwrap(),
            vec![
                (address.id, ProviderGroup::google(), 2),
                (address.id, ProviderGroup::yahoo(), 1)
            ]
        );

        // Optimistic concurrency on save.
        let mut state = store.warmup_states().await.unwrap()[0].clone();
        assert_eq!(state.provider, ProviderGroup::google());
        state.status = WarmupStatus::Paused;
        state.paused_until = Some(now + chrono::Duration::hours(1));
        assert!(store.save_warmup_state(&state, None).await.unwrap());
        assert!(
            !store.save_warmup_state(&state, None).await.unwrap(),
            "stale version"
        );
        let next_day = day.succ_opt().unwrap();
        assert!(!store
            .try_reserve_warmup(address.id, ProviderGroup::google(), next_day, &initial, now)
            .await
            .unwrap());

        // Settings: the address inherits the pool unless it sets its own.
        assert!(store
            .set_pool_warmup(pool.id, Some(WarmupMode::Slow), None)
            .await
            .unwrap());
        let listed = store.warmup_addresses().await.unwrap();
        assert_eq!(listed[0].effective_mode(), (WarmupMode::Slow, "pool"));
        assert!(store
            .set_address_warmup(address.id, Some(WarmupMode::Off), None)
            .await
            .unwrap());
        assert_eq!(
            store.warmup_addresses().await.unwrap()[0].effective_mode(),
            (WarmupMode::Off, "address")
        );
        assert!(!store.set_address_warmup(999, None, None).await.unwrap());
    }
}
