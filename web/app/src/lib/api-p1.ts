// P1 ("Activity") client helpers layered on top of the typed API client.
//
// Deliberately a separate file (like api-extras.ts): new clients and
// helpers land here so parallel feature branches do not contend over
// api.ts. Covers: tags, the API request log, windowed stats (turned into
// a chart time series client-side), per-message opens/clicks/raw, plus
// small presentation helpers (relative times, status-pill classes, MIME
// body extraction for the message-detail tabs).

import { api, type Pagination, type WindowStats } from "@/lib/api"

// ------------------------------------------------------------- types

/** One tag used by the server's messages in the last 30 days. */
export type TagCount = { tag: string; count: number }

/** One entry of the API request log (GET /api/v2/server/logs). */
export type ApiRequestEntry = {
  id: number
  method: string
  path: string
  status_code: number
  duration_ms: number
  user_agent: string | null
  created_at: string
}

/** One open/click tracking event of a message. */
export type MessageActivityEvent = {
  ip_address: string | null
  user_agent: string | null
  url: string | null
  created_at: string
}

/** Pending outbound queue depth (GET /api/v2/server/stats/deliveries). */
export type QueueDepth = { queued: number; domains: { domain: string; queued: number }[] }

// ----------------------------------------------------- server client

/** P1 additions to the per-server messaging API (X-Server-API-Key). */
export function serverApiP1(key: string) {
  const h = { "X-Server-API-Key": key }
  return {
    /** Tags used in the last 30 days, most used first. */
    tags: () => api.get<{ tags: TagCount[] }>("/api/v2/server/tags", h),
    /** The API request log, newest first. */
    logs: (params = "") =>
      api.get<{ requests: ApiRequestEntry[]; pagination: Pagination }>(
        `/api/v2/server/logs${params}`,
        h,
      ),
    /** Aggregate stats limited to a created_at window. */
    statsWindow: (from: Date, to: Date) =>
      api.get<{ stats: WindowStats }>(
        `/api/v2/server/stats?from=${encodeURIComponent(from.toISOString())}&to=${encodeURIComponent(to.toISOString())}`,
        h,
      ),
    /** Pending outbound queue depth. */
    queue: () => api.get<QueueDepth>("/api/v2/server/stats/deliveries", h),
    opens: (id: number) =>
      api.get<{ opens: MessageActivityEvent[] }>(`/api/v2/server/messages/${id}/opens`, h),
    clicks: (id: number) =>
      api.get<{ clicks: MessageActivityEvent[] }>(`/api/v2/server/messages/${id}/clicks`, h),
    /** The raw MIME, base64-encoded. 404 NotAvailable in privacy mode. */
    rawMime: (id: number) =>
      api.get<{ raw_message: string }>(`/api/v2/server/messages/${id}/raw`, h),
  }
}

// -------------------------------------------------- chart time series

/** One bucket of the delivery time series (all counts are real backend
 *  numbers — one windowed /stats call per bucket, no synthesis). */
export type DeliveryPoint = {
  /** Bucket start (ISO). */
  date: string
  /** Short axis label, e.g. "Jul 3". */
  label: string
  /** Outgoing messages created in the bucket. */
  sent: number
  delivered: number
  bounced: number
  /** Everything else still in flight: pending / held / soft+hard fails. */
  pending: number
  /** bounced / sent × 100, or null when nothing was sent. */
  bounceRate: number | null
}

const DAY_MS = 86_400_000

/** How many buckets a window is split into (bounds the request fan-out:
 *  one windowed /stats call per bucket). 7d stays daily; longer windows
 *  cap at 15 points. */
function bucketCount(days: number): number {
  return days <= 7 ? days : 15
}

/** The Sent/Delivered/Bounced series of the last `days` days, computed
 *  from windowed /stats calls (real backend numbers, one call per
 *  bucket — never synthesized). */
export async function deliveryTimeSeries(key: string, days: number): Promise<DeliveryPoint[]> {
  const p1 = serverApiP1(key)
  const buckets = bucketCount(days)
  const step = (days * DAY_MS) / buckets
  const end = Date.now()
  const start = end - days * DAY_MS
  const windows = Array.from({ length: buckets }, (_, i) => ({
    from: new Date(start + i * step),
    to: new Date(start + (i + 1) * step),
  }))
  const results = await Promise.all(
    windows.map((w) => p1.statsWindow(w.from, w.to).then((r) => r.stats)),
  )
  return results.map((s, i) => {
    const sent = s.outgoing
    const delivered = s.sent
    const bounced = s.bounced
    return {
      date: windows[i].from.toISOString(),
      label: windows[i].from.toLocaleDateString(undefined, { month: "short", day: "numeric" }),
      sent,
      delivered,
      bounced,
      pending: Math.max(0, sent - delivered - bounced),
      bounceRate: sent > 0 ? (bounced / sent) * 100 : null,
    }
  })
}

/** Deliverability early-warning thresholds (dashed RISK lines). */
export const RISK_BOUNCE_RATE_PCT = 4
export const RISK_COMPLAINT_RATE_PCT = 0.08

/** The validated chart palette (light/dark) for the delivery series. */
export const DELIVERY_SERIES_COLORS = {
  delivered: { light: "#1baf7a", dark: "#199e70" },
  pending: { light: "#c98500", dark: "#c98500" },
  bounced: { light: "#d03b3b", dark: "#e66767" },
} as const

// --------------------------------------------------- relative times

/** "just now" / "4m ago" / "3h ago" / "2d ago" / a date for older. */
export function relativeTime(value: string | null | undefined): string {
  if (!value) return "—"
  const date = new Date(value)
  if (Number.isNaN(date.getTime())) return value
  const seconds = Math.round((Date.now() - date.getTime()) / 1000)
  if (seconds < 45) return "just now"
  if (seconds < 90) return "1m ago"
  const minutes = Math.round(seconds / 60)
  if (minutes < 60) return `${minutes}m ago`
  const hours = Math.round(minutes / 60)
  if (hours < 24) return `${hours}h ago`
  const daysAgo = Math.round(hours / 24)
  if (daysAgo < 7) return `${daysAgo}d ago`
  return date.toLocaleDateString(undefined, { month: "short", day: "numeric", year: "numeric" })
}

// ------------------------------------------------------ status pills

/** The color semantics of lifecycle pills, consistent across Activity,
 *  message detail and the timeline: delivered green · bounced red ·
 *  held amber · queued gray · opened teal · clicked violet. Applied as
 *  classNames on the existing Badge component (variant="outline") —
 *  deliberately no status-pill.tsx component file. */
export const EVENT_PILL_CLASSES: Record<string, string> = {
  delivered:
    "border-green-600/30 bg-green-600/10 text-green-700 dark:border-green-400/30 dark:bg-green-400/10 dark:text-green-400",
  bounced:
    "border-red-600/30 bg-red-600/10 text-red-700 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-400",
  "hard fail":
    "border-red-600/30 bg-red-600/10 text-red-700 dark:border-red-400/30 dark:bg-red-400/10 dark:text-red-400",
  held: "border-amber-600/30 bg-amber-600/10 text-amber-700 dark:border-amber-400/30 dark:bg-amber-400/10 dark:text-amber-400",
  "soft fail":
    "border-amber-600/30 bg-amber-600/10 text-amber-700 dark:border-amber-400/30 dark:bg-amber-400/10 dark:text-amber-400",
  queued: "border-border bg-muted text-muted-foreground",
  sent: "border-border bg-muted text-muted-foreground",
  opened:
    "border-teal-600/30 bg-teal-600/10 text-teal-700 dark:border-teal-400/30 dark:bg-teal-400/10 dark:text-teal-400",
  clicked:
    "border-violet-600/30 bg-violet-600/10 text-violet-700 dark:border-violet-400/30 dark:bg-violet-400/10 dark:text-violet-400",
}

/** Map a message's lifecycle state to its event pill (label + classes). */
export function messageEventPill(message: {
  held?: boolean
  status?: string | null
}): { label: string; className: string } {
  if (message.held) return { label: "held", className: EVENT_PILL_CLASSES.held }
  switch (message.status) {
    case "Sent":
      return { label: "delivered", className: EVENT_PILL_CLASSES.delivered }
    case "Bounced":
      return { label: "bounced", className: EVENT_PILL_CLASSES.bounced }
    case "HardFail":
      return { label: "hard fail", className: EVENT_PILL_CLASSES["hard fail"] }
    case "SoftFail":
      return { label: "soft fail", className: EVENT_PILL_CLASSES["soft fail"] }
    case "Held":
      return { label: "held", className: EVENT_PILL_CLASSES.held }
    case "Pending":
    case null:
    case undefined:
      return { label: "queued", className: EVENT_PILL_CLASSES.queued }
    default:
      return { label: message.status.toLowerCase(), className: EVENT_PILL_CLASSES.queued }
  }
}

/** Traffic-light pill classes for an HTTP status code (request log). */
export function httpStatusPillClass(code: number): string {
  if (code >= 500) return EVENT_PILL_CLASSES.bounced
  if (code >= 400) return EVENT_PILL_CLASSES.held
  if (code >= 300) return EVENT_PILL_CLASSES.queued
  return EVENT_PILL_CLASSES.delivered
}

// --------------------------------------------------- MIME extraction

/** The display-relevant parts of a raw RFC 5322 message. */
export type ParsedEmail = {
  /** Top-level headers, lowercased names, first value each. */
  headers: Record<string, string>
  textBody: string | null
  htmlBody: string | null
  attachments: { filename: string | null; contentType: string }[]
}

function base64ToBytes(b64: string): Uint8Array {
  const binary = atob(b64.replace(/\s+/g, ""))
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
  return bytes
}

/** Bytes → string, honoring the declared charset (utf-8 fallback). */
function decodeBytes(bytes: Uint8Array, charset: string | null): string {
  try {
    return new TextDecoder(charset || "utf-8").decode(bytes)
  } catch {
    return new TextDecoder("utf-8").decode(bytes)
  }
}

/** The latin1 view of bytes — structure-safe for header/boundary work. */
function bytesToLatin1(bytes: Uint8Array): string {
  let out = ""
  const chunk = 0x8000
  for (let i = 0; i < bytes.length; i += chunk) {
    out += String.fromCharCode(...bytes.subarray(i, i + chunk))
  }
  return out
}

function latin1ToBytes(s: string): Uint8Array {
  return Uint8Array.from(s, (c) => c.charCodeAt(0) & 0xff)
}

/** Parse a header block into a lowercased name → unfolded value map. */
function parseHeaders(block: string): Record<string, string> {
  const headers: Record<string, string> = {}
  const unfolded = block.replace(/\r?\n[ \t]+/g, " ")
  for (const line of unfolded.split(/\r?\n/)) {
    const colon = line.indexOf(":")
    if (colon <= 0) continue
    const name = line.slice(0, colon).trim().toLowerCase()
    const value = line.slice(colon + 1).trim()
    if (!(name in headers)) headers[name] = value
  }
  return headers
}

/** One parameter (boundary, charset, filename…) of a structured header. */
function headerParam(value: string | undefined, param: string): string | null {
  if (!value) return null
  const match = value.match(new RegExp(`${param}\\s*=\\s*("([^"]*)"|[^;\\s]+)`, "i"))
  if (!match) return null
  return match[2] ?? match[1]
}

function decodeQuotedPrintable(body: string): Uint8Array {
  const cleaned = body.replace(/=\r?\n/g, "")
  const bytes: number[] = []
  for (let i = 0; i < cleaned.length; i++) {
    if (cleaned[i] === "=" && /^[0-9a-fA-F]{2}$/.test(cleaned.slice(i + 1, i + 3))) {
      bytes.push(parseInt(cleaned.slice(i + 1, i + 3), 16))
      i += 2
    } else {
      bytes.push(cleaned.charCodeAt(i) & 0xff)
    }
  }
  return new Uint8Array(bytes)
}

/** Decode RFC 2047 encoded words in a header value ("=?utf-8?B?...?="). */
export function decodeHeaderValue(value: string): string {
  return value.replace(
    /=\?([^?]+)\?([bBqQ])\?([^?]*)\?=/g,
    (_all, charset: string, enc: string, text: string) => {
      try {
        const bytes =
          enc.toLowerCase() === "b"
            ? base64ToBytes(text)
            : decodeQuotedPrintable(text.replace(/_/g, " "))
        return decodeBytes(bytes, charset)
      } catch {
        return text
      }
    },
  )
}

function parseEntity(raw: string, out: ParsedEmail, depth: number): Record<string, string> {
  const split = raw.match(/\r?\n\r?\n/)
  const headerBlock = split ? raw.slice(0, split.index) : raw
  const body = split ? raw.slice((split.index ?? 0) + split[0].length) : ""
  const headers = parseHeaders(headerBlock)
  const contentType = headers["content-type"] ?? "text/plain"
  const mediaType = contentType.split(";")[0].trim().toLowerCase()

  if (mediaType.startsWith("multipart/") && depth < 8) {
    const boundary = headerParam(contentType, "boundary")
    if (boundary) {
      const marker = `--${boundary}`
      const sections = body.split(new RegExp(`(?:^|\r?\n)${escapeRegExp(marker)}`))
      // sections[0] is the preamble; a part ending with `--` is the close
      for (const section of sections.slice(1)) {
        if (section.startsWith("--")) break
        parseEntity(section.replace(/^\r?\n/, ""), out, depth + 1)
      }
    }
    return headers
  }

  const disposition = headers["content-disposition"] ?? ""
  const filename =
    headerParam(disposition, "filename") ?? headerParam(contentType, "name")
  const isAttachment = /^\s*attachment/i.test(disposition) || (!!filename && depth > 0)

  if (isAttachment) {
    out.attachments.push({
      filename: filename ? decodeHeaderValue(filename) : null,
      contentType: mediaType,
    })
    return headers
  }

  if (mediaType === "text/plain" || mediaType === "text/html") {
    const encoding = (headers["content-transfer-encoding"] ?? "").trim().toLowerCase()
    const charset = headerParam(contentType, "charset")
    let bytes: Uint8Array
    if (encoding === "base64") {
      try {
        bytes = base64ToBytes(body)
      } catch {
        bytes = latin1ToBytes(body)
      }
    } else if (encoding === "quoted-printable") {
      bytes = decodeQuotedPrintable(body)
    } else {
      bytes = latin1ToBytes(body)
    }
    const text = decodeBytes(bytes, charset)
    if (mediaType === "text/plain" && out.textBody === null) out.textBody = text
    if (mediaType === "text/html" && out.htmlBody === null) out.htmlBody = text
  }
  return headers
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")
}

/** Extract headers, display bodies and attachments from the base64 raw
 *  MIME returned by GET /messages/{id}/raw. Best-effort: unknown
 *  structures degrade to nulls, never to an exception. */
export function parseRawEmail(base64: string): ParsedEmail {
  const out: ParsedEmail = { headers: {}, textBody: null, htmlBody: null, attachments: [] }
  try {
    const raw = bytesToLatin1(base64ToBytes(base64))
    out.headers = parseEntity(raw, out, 0)
    for (const name of Object.keys(out.headers)) {
      out.headers[name] = decodeHeaderValue(out.headers[name])
    }
  } catch {
    // leave the empty result — the UI shows "not available"
  }
  return out
}
