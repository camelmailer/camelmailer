// IP metrics (GET /api/v2/admin/ip_metrics) and the adaptive warm-up
// (GET /api/v2/admin/ip_warmup, …/log, PATCH …/addresses/{id} and
// …/pools/{id}). Types, the API calls and the pure helpers the page uses;
// the helpers are unit-tested in warmup.test.ts.

import { api } from "@/lib/api"

// ------------------------------------------------------------------ types

/** A provider group name: built in microsoft, google, yahoo, apple,
 * united_internet, t_online and other; more can be configured. */
export type Provider = string

export type MetricTotals = {
  sent: number
  deferred: number
  bounced: number
  policy_blocks: number
  complaints: number
  attempts: number
  deferral_rate: number | null
  bounce_rate: number | null
  policy_block_rate: number | null
  complaint_rate: number | null
}

export type AddressMetrics = {
  source_ip: string
  ip_address_id: number | null
  default_source: boolean
  pool: { id: number; name: string } | null
  hostname: string | null
  totals: MetricTotals
  today: MetricTotals
  providers: (MetricTotals & { provider: Provider })[]
  daily: (MetricTotals & { day: string })[]
}

export type IpMetrics = {
  days: number
  since: string
  until: string
  addresses: AddressMetrics[]
  pools: { id: number; name: string; totals: MetricTotals }[]
}

export type WarmupMode = "off" | "slow" | "normal" | "fast" | "custom"
export type WarmupStatus = "not_started" | "ramping" | "holding" | "slowed" | "paused" | "warm"
export type WarmupProfile = {
  start: number
  growth: number
  cap: number
  groups?: Record<string, { start: number; cap: number }>
}

export type WarmupLogEntry = {
  ip_address_id: number
  provider: Provider | null
  decision: "advance" | "hold" | "slow" | "brake" | "warm" | "mode" | "storage"
  reason: string
  allowance_before: number | null
  allowance_after: number | null
  created_at: string
}

export type WarmupProviderState = {
  provider: Provider
  allowance: number | null
  used_today: number
  status: WarmupStatus | null
  paused: boolean
  paused_until: string | null
  last_reason: string | null
  last_brake_reason: string | null
  last_brake_at: string | null
  evaluated_day: string | null
  progress: number | null
  eta_days: number | null
}

export type WarmupAddress = {
  ip_address_id: number
  ipv4: string
  hostname: string
  pool: { id: number; name: string; mode: WarmupMode | null }
  mode: WarmupMode
  mode_source: "address" | "pool" | "default"
  address_mode: WarmupMode | null
  active: boolean
  profile: WarmupProfile | null
  providers: WarmupProviderState[]
  eta_days: number | null
  recent_decisions: WarmupLogEntry[]
}

export type Warmup = {
  profiles: Record<"slow" | "normal" | "fast", WarmupProfile>
  thresholds: Record<string, number>
  provider_groups: Provider[]
  storage_incidents: WarmupLogEntry[]
  addresses: WarmupAddress[]
}

export type SetWarmup = { mode: WarmupMode | "inherit"; profile?: WarmupProfile }

export const warmupApi = {
  metrics: (days: number) => api.get<IpMetrics>(`/api/v2/admin/ip_metrics?days=${days}`),
  warmup: () => api.get<Warmup>("/api/v2/admin/ip_warmup"),
  log: (ipAddressId?: number) =>
    api.get<{ log: WarmupLogEntry[] }>(
      `/api/v2/admin/ip_warmup/log${ipAddressId ? `?ip_address_id=${ipAddressId}` : ""}`,
    ),
  setAddress: (id: number, body: SetWarmup) =>
    api.patch<{ address: WarmupAddress }>(`/api/v2/admin/ip_warmup/addresses/${id}`, body),
  setPool: (id: number, body: SetWarmup) =>
    api.patch<{ ip_pool: { id: number; mode: WarmupMode | null } }>(
      `/api/v2/admin/ip_warmup/pools/${id}`,
      body,
    ),
}

// ---------------------------------------------------------------- helpers

const PROVIDER_LABELS: Record<string, string> = {
  microsoft: "Microsoft",
  google: "Google",
  yahoo: "Yahoo",
  apple: "Apple",
  united_internet: "United Internet",
  t_online: "T-Online",
  other: "Others",
}

/** The display name of a provider group; configured groups show their name. */
export function providerLabel(provider: Provider): string {
  return PROVIDER_LABELS[provider] ?? provider.replace(/_/g, " ")
}

export const MODE_LABELS: Record<WarmupMode, string> = {
  off: "Off",
  slow: "Slow",
  normal: "Normal",
  fast: "Fast",
  custom: "Custom",
}

export const STATUS_LABELS: Record<WarmupStatus, string> = {
  not_started: "not started",
  ramping: "ramping",
  holding: "holding",
  slowed: "slowed",
  paused: "paused",
  warm: "warm",
}

export const STATUS_TONE: Record<WarmupStatus, "green" | "amber" | "red" | "gray" | "teal"> = {
  not_started: "gray",
  ramping: "teal",
  holding: "gray",
  slowed: "amber",
  paused: "red",
  warm: "green",
}

/** A rate as a percentage with two decimals, or "—" without data. */
export function formatRate(rate: number | null | undefined): string {
  if (rate === null || rate === undefined) return "—"
  return `${(rate * 100).toFixed(2)} %`
}

/** Whether a rate crosses a threshold (warn / brake colouring). */
export function rateLevel(
  rate: number | null | undefined,
  warn: number,
  bad: number,
): "ok" | "warn" | "bad" {
  if (rate === null || rate === undefined) return "ok"
  if (rate >= bad) return "bad"
  if (rate >= warn) return "warn"
  return "ok"
}

/** "12 days", "1 day", "warm" or "—". */
export function etaText(state: { eta_days: number | null; status: WarmupStatus | null }): string {
  if (state.status === "warm") return "warm"
  if (state.eta_days === null) return "—"
  if (state.eta_days === 0) return "warm"
  return `${state.eta_days} ${state.eta_days === 1 ? "day" : "days"}`
}

/** Today's use against the allowance, 0 to 100, for a progress bar. */
export function usedPercent(state: { used_today: number; allowance: number | null }): number {
  if (!state.allowance) return 0
  return Math.min(100, Math.round((state.used_today / state.allowance) * 100))
}

/** The value of the mode selector: the address's own mode, or "inherit". */
export function selectorValue(address: WarmupAddress): WarmupMode | "inherit" {
  return address.address_mode ?? "inherit"
}

/** A PATCH body from the selector and the custom-profile inputs; null when
 * a custom profile is incomplete or out of range (the API would refuse it). */
export function warmupBody(
  mode: WarmupMode | "inherit",
  custom: { start: string; growth: string; cap: string },
): SetWarmup | null {
  if (mode !== "custom") return { mode }
  const start = Number(custom.start)
  const growth = Number(custom.growth)
  const cap = Number(custom.cap)
  if (!Number.isInteger(start) || start < 1 || start > 1_000_000) return null
  if (!(growth >= 1.05 && growth <= 3)) return null
  if (!Number.isInteger(cap) || cap <= start || cap > 10_000_000) return null
  return { mode, profile: { start, growth, cap } }
}

/** The most recent brake of an address over all providers, if any. */
export function lastBrake(address: WarmupAddress): { provider: Provider; reason: string; at: string } | null {
  let latest: { provider: Provider; reason: string; at: string } | null = null
  for (const p of address.providers) {
    if (p.last_brake_reason && p.last_brake_at) {
      if (!latest || p.last_brake_at > latest.at) {
        latest = { provider: p.provider, reason: p.last_brake_reason, at: p.last_brake_at }
      }
    }
  }
  return latest
}
