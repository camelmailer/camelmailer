//! `GET /api/v2/admin/ip_metrics`: delivery counters per sending address,
//! per receiving provider and per IP pool, for the infrastructure dashboard.
//! Installation-wide, so global administrators only (like `ip_pools`).

use crate::app::{render_store_error, render_success, ApiResponse, ApiState, RequestStart};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use camelmailer_core::{IpDeliveryStat, ProviderGroup};
use chrono::{Duration, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// The longest window the endpoint answers for.
const MAX_DAYS: i64 = 90;

#[derive(Debug, Deserialize)]
pub(crate) struct IpMetricsParams {
    /// Days to cover, today included (1..=90, default 7).
    pub days: Option<i64>,
}

/// Summed counters with the rates the dashboard shows.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Totals {
    pub sent: i64,
    pub deferred: i64,
    pub bounced: i64,
    pub policy_blocks: i64,
    pub complaints: i64,
}

impl Totals {
    fn add(&mut self, stat: &IpDeliveryStat) {
        self.sent += stat.sent;
        self.deferred += stat.deferred;
        self.bounced += stat.bounced;
        self.policy_blocks += stat.policy_blocks;
        self.complaints += stat.complaints;
    }

    fn attempts(&self) -> i64 {
        self.sent + self.deferred + self.bounced
    }

    fn rate(part: i64, whole: i64) -> Option<f64> {
        (whole > 0).then(|| (part as f64 / whole as f64 * 10_000.0).round() / 10_000.0)
    }

    pub fn to_json(self) -> Value {
        let attempts = self.attempts();
        json!({
            "sent": self.sent,
            "deferred": self.deferred,
            "bounced": self.bounced,
            "policy_blocks": self.policy_blocks,
            "complaints": self.complaints,
            "attempts": attempts,
            // Shares of all attempts; the complaint rate is per accepted
            // message, the way mailbox providers state it.
            "deferral_rate": Self::rate(self.deferred, attempts),
            "bounce_rate": Self::rate(self.bounced, attempts),
            "policy_block_rate": Self::rate(self.policy_blocks, attempts),
            "complaint_rate": Self::rate(self.complaints, self.sent),
        })
    }
}

/// The address a pool row stands for, keyed by its IPv4 value (the source
/// key the worker counts under).
struct KnownAddress {
    pool_id: u64,
    pool_name: String,
    hostname: String,
}

pub(crate) async fn ip_metrics_index(
    State(state): State<Arc<ApiState>>,
    start: axum::Extension<RequestStart>,
    Query(params): Query<IpMetricsParams>,
) -> ApiResponse {
    let days = params.days.unwrap_or(7).clamp(1, MAX_DAYS);
    let today = Utc::now().date_naive();
    let since = today - Duration::days(days - 1);

    let stats = match &state.delivery_stats {
        Some(store) => match store.ip_delivery_stats(since).await {
            Ok(stats) => stats,
            Err(error) => return render_store_error(Some(&start), error),
        },
        None => Vec::new(),
    };

    let mut known: BTreeMap<String, KnownAddress> = BTreeMap::new();
    let pools = match state.store.list_ip_pools().await {
        Ok(pools) => pools,
        Err(error) => return render_store_error(Some(&start), error),
    };
    for pool in &pools {
        let addresses = match state.store.list_ip_addresses(pool.id).await {
            Ok(addresses) => addresses,
            Err(error) => return render_store_error(Some(&start), error),
        };
        for address in addresses {
            known.insert(
                address.ipv4.clone(),
                KnownAddress {
                    pool_id: pool.id,
                    pool_name: pool.name.clone(),
                    hostname: address.hostname.clone(),
                },
            );
        }
    }

    render_success(
        Some(&start),
        StatusCode::OK,
        summarize(&stats, &known, since, today, days),
    )
}

fn summarize(
    stats: &[IpDeliveryStat],
    known: &BTreeMap<String, KnownAddress>,
    since: NaiveDate,
    today: NaiveDate,
    days: i64,
) -> Value {
    #[derive(Default)]
    struct PerAddress {
        ip_address_id: Option<u64>,
        totals: Totals,
        today: Totals,
        providers: BTreeMap<ProviderGroup, Totals>,
        daily: BTreeMap<NaiveDate, Totals>,
    }
    let mut addresses: BTreeMap<String, PerAddress> = BTreeMap::new();
    let mut pools: BTreeMap<u64, (String, Totals)> = BTreeMap::new();
    for stat in stats {
        let entry = addresses.entry(stat.source_ip.clone()).or_default();
        entry.ip_address_id = entry.ip_address_id.or(stat.ip_address_id);
        entry.totals.add(stat);
        if stat.day == today {
            entry.today.add(stat);
        }
        entry.providers.entry(stat.provider).or_default().add(stat);
        entry.daily.entry(stat.day).or_default().add(stat);
        if let Some(address) = known.get(&stat.source_ip) {
            pools
                .entry(address.pool_id)
                .or_insert_with(|| (address.pool_name.clone(), Totals::default()))
                .1
                .add(stat);
        }
    }
    let addresses: Vec<Value> = addresses
        .into_iter()
        .map(|(source_ip, entry)| {
            let address = known.get(&source_ip);
            json!({
                "source_ip": source_ip,
                "ip_address_id": entry.ip_address_id,
                "default_source": source_ip == camelmailer_core::DEFAULT_SOURCE,
                "pool": address.map(|a| json!({ "id": a.pool_id, "name": a.pool_name })),
                "hostname": address.map(|a| a.hostname.clone()),
                "totals": entry.totals.to_json(),
                "today": entry.today.to_json(),
                "providers": entry.providers.iter().map(|(provider, totals)| {
                    let mut value = totals.to_json();
                    value["provider"] = json!(provider.as_str());
                    value
                }).collect::<Vec<_>>(),
                "daily": entry.daily.iter().map(|(day, totals)| {
                    let mut value = totals.to_json();
                    value["day"] = json!(day.to_string());
                    value
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    let pools: Vec<Value> = pools
        .into_iter()
        .map(|(id, (name, totals))| json!({ "id": id, "name": name, "totals": totals.to_json() }))
        .collect();
    json!({
        "days": days,
        "since": since.to_string(),
        "until": today.to_string(),
        "addresses": addresses,
        "pools": pools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stat(
        day: NaiveDate,
        ip: &str,
        provider: ProviderGroup,
        sent: i64,
        deferred: i64,
    ) -> IpDeliveryStat {
        IpDeliveryStat {
            day,
            source_ip: ip.into(),
            ip_address_id: Some(1),
            provider,
            sent,
            deferred,
            bounced: 0,
            policy_blocks: 0,
            complaints: 0,
        }
    }

    #[test]
    fn counters_add_up_per_address_provider_day_and_pool() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let yesterday = today.pred_opt().unwrap();
        let mut known = BTreeMap::new();
        known.insert(
            "203.0.113.5".to_string(),
            KnownAddress {
                pool_id: 7,
                pool_name: "Main".into(),
                hostname: "out1.example.com".into(),
            },
        );
        let mut complained = stat(today, "203.0.113.5", ProviderGroup::Google, 0, 0);
        complained.complaints = 1;
        let stats = vec![
            stat(yesterday, "203.0.113.5", ProviderGroup::Microsoft, 90, 10),
            stat(today, "203.0.113.5", ProviderGroup::Google, 100, 0),
            complained,
            stat(today, "default", ProviderGroup::Other, 5, 0),
        ];
        let value = summarize(&stats, &known, yesterday, today, 2);
        let first = &value["addresses"][0];
        assert_eq!(first["source_ip"], "203.0.113.5");
        assert_eq!(first["pool"]["name"], "Main");
        assert_eq!(first["totals"]["attempts"], 200);
        assert_eq!(first["totals"]["deferral_rate"], 0.05);
        assert_eq!(first["totals"]["complaint_rate"], 0.0053);
        assert_eq!(first["today"]["sent"], 100);
        assert_eq!(first["providers"].as_array().unwrap().len(), 2);
        assert_eq!(first["daily"][0]["day"], "2026-11-13");
        let default = &value["addresses"][1];
        assert_eq!(default["default_source"], true);
        assert_eq!(default["pool"], Value::Null);
        assert_eq!(value["pools"][0]["totals"]["sent"], 190);
        assert_eq!(value["pools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn rates_are_null_without_attempts() {
        let value = Totals::default().to_json();
        assert_eq!(value["deferral_rate"], Value::Null);
        assert_eq!(value["complaint_rate"], Value::Null);
    }
}
