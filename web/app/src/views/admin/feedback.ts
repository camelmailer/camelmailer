// Judgment for the Provider feedback page, kept out of the view so it can
// be tested on its own.

import type { FeedbackSource, FeedbackVerdict, PlacementTest } from "@/lib/api"

export const SOURCE_LABELS: Record<FeedbackSource["source"], { title: string; provider: string }> = {
  postmaster_tools: { title: "Google Postmaster Tools", provider: "Gmail" },
  snds: { title: "Microsoft SNDS", provider: "Outlook.com" },
  fbl: { title: "Complaint feedback loops", provider: "Microsoft JMRP, Yahoo CFL" },
  placement: { title: "Seed placement", provider: "Your seed mailboxes" },
}

export const PROVIDER_LABELS: Record<string, string> = {
  google: "Google",
  microsoft: "Microsoft",
  yahoo: "Yahoo / AOL",
  apple: "Apple iCloud",
  united_internet: "GMX / Web.de / 1&1",
  t_online: "T-Online",
  other: "Other",
}

/** A provider group's label; configured groups without one read as named. */
export function providerLabel(group: string): string {
  return PROVIDER_LABELS[group] ?? group.replaceAll("_", " ")
}

/** The pill tone of a verdict. */
export function verdictTone(verdict: FeedbackVerdict): "green" | "amber" | "red" | "gray" {
  switch (verdict) {
    case "ok":
      return "green"
    case "warn":
      return "amber"
    case "bad":
      return "red"
    default:
      return "gray"
  }
}

export const VERDICT_LABELS: Record<FeedbackVerdict, string> = {
  ok: "Healthy",
  warn: "Watch",
  bad: "Act now",
  unknown: "No verdict",
}

/** "3 h", "2 d": the age of the newest data, or null without any. */
export function age(seconds: number | null): string | null {
  if (seconds == null) return null
  if (seconds < 3600) return `${Math.max(1, Math.round(seconds / 60))} min`
  if (seconds < 172_800) return `${Math.round(seconds / 3600)} h`
  return `${Math.round(seconds / 86_400)} d`
}

/**
 * How a source is doing: off (no credentials), failing (the last fetch
 * failed), stale (older than two days; both providers publish daily),
 * waiting (configured, nothing fetched yet) or fresh.
 */
export function sourceState(
  source: FeedbackSource,
): "off" | "failing" | "stale" | "waiting" | "fresh" {
  if (!source.enabled) return "off"
  if (source.status?.last_error) return "failing"
  if (source.freshness_seconds == null) return "waiting"
  // A feedback loop is quiet when nobody complains, which is good news, and
  // placement rounds run as often as the operator asks.
  const quietIsFine = source.source === "fbl" || source.source === "placement"
  if (!quietIsFine && source.freshness_seconds > 172_800) return "stale"
  return "fresh"
}

/** The worst verdict among a source's snapshots. */
export function worstVerdict(source: FeedbackSource): FeedbackVerdict {
  const order: FeedbackVerdict[] = ["unknown", "ok", "warn", "bad"]
  return source.snapshots.reduce<FeedbackVerdict>(
    (worst, s) => (order.indexOf(s.verdict) > order.indexOf(worst) ? s.verdict : worst),
    "unknown",
  )
}

/** A metric as a percentage when it is a rate (0..1), else as is. */
export function formatMetric(key: string, value: unknown): string {
  if (value == null) return "—"
  if (typeof value === "number" && key.endsWith("_rate")) {
    return `${(value * 100).toFixed(2)} %`
  }
  return String(value)
}

/** The pill of a placement result. */
export function placementTone(status: PlacementTest["status"]): {
  label: string
  tone: "green" | "amber" | "red" | "gray"
} {
  switch (status) {
    case "inbox":
      return { label: "Inbox", tone: "green" }
    case "spam":
      return { label: "Spam", tone: "red" }
    case "missing":
      return { label: "Missing", tone: "amber" }
    case "error":
      return { label: "Error", tone: "red" }
    default:
      return { label: "Waiting", tone: "gray" }
  }
}

/** `spf pass · dkim pass · dmarc fail`, or null without any result. */
export function authSummary(test: Pick<PlacementTest, "spf" | "dkim" | "dmarc">): string | null {
  const parts = (["spf", "dkim", "dmarc"] as const)
    .filter((key) => test[key])
    .map((key) => `${key} ${test[key]}`)
  return parts.length ? parts.join(" · ") : null
}
