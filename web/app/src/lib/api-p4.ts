// P4 ("Polish & depth") client helpers layered on top of the typed API
// client. Like api-extras / api-p1 / api-p2 / api-p3, this is a separate
// file so parallel feature branches never contend over api.ts.
//
// Covers three P4 surfaces:
//   * Statistics view helpers (bounce breakdown, sentence-KPI maths) —
//     reuses the windowed /stats client from api-p1 (never synthesizes).
//   * Usage aggregation for the org billing/usage view (real "sent" over
//     the last 30 days, summed across a server's credentials).
//   * The `</>` code-panel catalog: a small, OpenAPI-shaped descriptor of
//     the public endpoints, rendered to copy-paste snippets in four
//     languages. Deliberately a static map (no OpenAPI parser) — the
//     descriptors mirror public/openapi.yaml (paths, methods, examples).

import { adminApi, type Role } from "@/lib/api"
import { serverApiP1 } from "@/lib/api-p1"
import { type WindowStats } from "@/lib/api"
import { firstApiCredentialKey } from "@/lib/api-extras"

// --------------------------------------------------------- statistics

const DAY_MS = 86_400_000

/** Aggregate /stats counters over a created_at window (real backend
 *  numbers — one call, includes the bounce breakdown when the server
 *  classified any). */
export async function statsForRange(
  key: string,
  from: Date,
  to: Date,
): Promise<WindowStats> {
  const { stats } = await serverApiP1(key).statsWindow(from, to)
  return stats
}

/** The three bounce classes as a normalized, click-through-ready list.
 *  Falls back to the flat hard/soft counters when the server did not send
 *  the richer `bounces` object (older backends), and derives
 *  "undetermined" as the remainder so the parts always sum to `bounced`. */
export type BounceSlice = {
  key: "hard" | "soft" | "undetermined"
  label: string
  count: number
  /** Message-list query string that isolates this class (deep link). */
  query: string
}

export function bounceBreakdown(stats: WindowStats): {
  total: number
  slices: BounceSlice[]
} {
  const hard = stats.bounces?.hard ?? stats.hard_fail
  const soft = stats.bounces?.soft ?? stats.soft_fail
  const undetermined =
    stats.bounces?.undetermined ?? Math.max(0, stats.bounced - hard - soft)
  const total = hard + soft + undetermined
  const slices: BounceSlice[] = [
    { key: "hard", label: "Hard bounces", count: hard, query: "?status=HardFail" },
    { key: "soft", label: "Soft bounces", count: soft, query: "?status=SoftFail" },
    {
      key: "undetermined",
      label: "Undetermined",
      count: undetermined,
      query: "?status=Bounced",
    },
  ]
  return { total, slices }
}

/** part / whole × 100, or null when the denominator is zero. */
export function ratePct(part: number, whole: number): number | null {
  return whole > 0 ? (part / whole) * 100 : null
}

export function formatPct(value: number | null): string {
  if (value == null) return "—"
  return `${value.toFixed(value > 0 && value < 10 ? 2 : 1)}%`
}

/** Semantic colors for the bounce breakdown + opened/clicked donut,
 *  consistent with the event-pill palette (red / amber / gray, teal /
 *  violet). Light + dark tuned. */
export const STAT_COLORS = {
  hard: { light: "#d03b3b", dark: "#e66767" },
  soft: { light: "#c98500", dark: "#d69a1f" },
  undetermined: { light: "#94a3b8", dark: "#64748b" },
  opened: { light: "#0d9488", dark: "#2dd4bf" },
  clicked: { light: "#7c3aed", dark: "#a78bfa" },
  neither: { light: "#e2e8f0", dark: "#334155" },
} as const

// -------------------------------------------------------------- usage

export type OrgUsage = {
  /** Outgoing messages created in the last 30 days, summed across every
   *  server the caller can read (via each server's first API credential).
   *  null when no server exposed a usable credential — then the UI shows a
   *  "connect a credential" hint rather than a fabricated zero. */
  sent30d: number | null
  /** Per-server contribution, for a small breakdown table. */
  perServer: { name: string; permalink: string; sent: number | null }[]
}

/** Real 30-day outbound volume for the whole organization. */
export async function orgUsage(org: string): Promise<OrgUsage> {
  const { servers } = await adminApi.servers(org).list()
  const to = new Date()
  const from = new Date(to.getTime() - 30 * DAY_MS)
  const perServer = await Promise.all(
    servers.map(async (server) => {
      const key = await firstApiCredentialKey(org, server.permalink)
      if (!key) return { name: server.name, permalink: server.permalink, sent: null }
      try {
        const stats = await statsForRange(key, from, to)
        return { name: server.name, permalink: server.permalink, sent: stats.outgoing }
      } catch {
        return { name: server.name, permalink: server.permalink, sent: null }
      }
    }),
  )
  const known = perServer.filter((s) => s.sent != null)
  const sent30d = known.length ? known.reduce((sum, s) => sum + (s.sent ?? 0), 0) : null
  return { sent30d, perServer }
}

/** Billing is only surfaced to admins and owners (never viewers/members). */
export function canManageBilling(role: Role | "root" | null | undefined): boolean {
  return role === "root" || role === "owner" || role === "admin"
}

// -------------------------------------------------- code-panel catalog

/** One documented endpoint the `</>` panel can show snippets for.
 *  Mirrors public/openapi.yaml (single source in spirit; static in form
 *  so no YAML parser ships to the client). */
export type ApiEndpoint = {
  id: string
  label: string
  description: string
  method: "GET" | "POST" | "PATCH" | "DELETE"
  /** Path with `:param` placeholders resolved against the current route. */
  path: string
  auth: "server" | "admin"
  /** JSON request body example (POST/PATCH). */
  body?: unknown
  /** SDK-style Node expression, e.g. `camelmailer.emails.send({ … })`. */
  sdk?: string
}

export type CodeLang = "curl" | "node" | "python" | "go"

export const CODE_LANGS: { value: CodeLang; label: string }[] = [
  { value: "curl", label: "cURL" },
  { value: "node", label: "Node" },
  { value: "python", label: "Python" },
  { value: "go", label: "Go" },
]

const LANG_STORAGE_KEY = "camelmailer.code_panel_lang"

export function loadCodeLang(): CodeLang {
  if (typeof window === "undefined") return "curl"
  const stored = window.localStorage.getItem(LANG_STORAGE_KEY)
  return CODE_LANGS.some((l) => l.value === stored) ? (stored as CodeLang) : "curl"
}

export function saveCodeLang(lang: CodeLang) {
  if (typeof window !== "undefined") window.localStorage.setItem(LANG_STORAGE_KEY, lang)
}

/** Context resolved from the active route + a masked key placeholder. */
export type CodeContext = { baseUrl: string; org: string; server: string }

/** The static catalog, keyed by section id. */
export function endpointCatalog(): Record<string, ApiEndpoint> {
  return {
    send: {
      id: "send",
      label: "Send an email",
      description: "Queue one message. The from-domain must be a verified sending domain.",
      method: "POST",
      path: "/api/v2/server/messages",
      auth: "server",
      body: {
        from: "billing@acme.com",
        to: ["ada@example.com"],
        subject: "Your receipt",
        text_body: "Thanks for your purchase.",
        tag: "receipt",
      },
      sdk: "camelmailer.emails.send",
    },
    messages: {
      id: "messages",
      label: "List messages",
      description: "Page through delivered, held and bounced mail with filters.",
      method: "GET",
      path: "/api/v2/server/messages?scope=outgoing&per_page=25",
      auth: "server",
      sdk: "camelmailer.messages.list",
    },
    templates: {
      id: "templates",
      label: "List templates",
      description: "The server's transactional templates (draft + published).",
      method: "GET",
      path: "/api/v2/server/templates",
      auth: "server",
      sdk: "camelmailer.templates.list",
    },
    stats: {
      id: "stats",
      label: "Server statistics",
      description: "Aggregate counters: sent, bounced (by class), opens and clicks.",
      method: "GET",
      path: "/api/v2/server/stats",
      auth: "server",
      sdk: "camelmailer.stats.get",
    },
    webhooks: {
      id: "webhooks",
      label: "List webhooks",
      description: "Delivery webhooks configured for this server.",
      method: "GET",
      path: "/api/v2/admin/organizations/:org/servers/:server/webhooks",
      auth: "admin",
    },
    domains: {
      id: "domains",
      label: "List domains",
      description: "Sending domains with their DKIM / return-path / verification state.",
      method: "GET",
      path: "/api/v2/admin/organizations/:org/servers/:server/domains",
      auth: "admin",
    },
  }
}

/** Which catalog section fits the current path (the UI-as-API twin). */
export function resolveApiSection(pathname: string): string {
  if (pathname.includes("/webhooks")) return "webhooks"
  if (pathname.includes("/domains")) return "domains"
  if (pathname.includes("/templates")) return "templates"
  if (pathname.includes("/messaging")) return "messages"
  // Send is the hero surface and the default for the messaging root.
  return "send"
}

function resolvePath(ep: ApiEndpoint, ctx: CodeContext): string {
  return ep.path.replace(":org", ctx.org || "ORG").replace(":server", ctx.server || "SERVER")
}

const KEY_PLACEHOLDER = "cm_your_api_key"

function authHeaderPair(ep: ApiEndpoint): [string, string] {
  return ep.auth === "server"
    ? ["X-Server-API-Key", KEY_PLACEHOLDER]
    : ["Authorization", `Bearer ${KEY_PLACEHOLDER}`]
}

/** Render an endpoint to a copy-paste snippet in the requested language. */
export function renderSnippet(ep: ApiEndpoint, lang: CodeLang, ctx: CodeContext): string {
  const path = resolvePath(ep, ctx)
  const url = `${ctx.baseUrl}${path}`
  const [headerName, headerValue] = authHeaderPair(ep)
  const bodyJson = ep.body ? JSON.stringify(ep.body, null, 2) : null

  switch (lang) {
    case "curl": {
      const lines = [`curl -X ${ep.method} "${url}" \\`, `  -H "${headerName}: ${headerValue}"`]
      if (bodyJson) {
        lines[lines.length - 1] += ` \\`
        lines.push(`  -H "Content-Type: application/json" \\`)
        lines.push(`  -d '${JSON.stringify(ep.body)}'`)
      }
      return lines.join("\n")
    }
    case "node": {
      if (ep.sdk && ep.method === "POST" && ep.body) {
        return [
          `import { CamelMailer } from "camelmailer"`,
          ``,
          `const camelmailer = new CamelMailer("${KEY_PLACEHOLDER}")`,
          ``,
          `await ${ep.sdk}(${bodyJson})`,
        ].join("\n")
      }
      if (ep.sdk) {
        return [
          `import { CamelMailer } from "camelmailer"`,
          ``,
          `const camelmailer = new CamelMailer("${KEY_PLACEHOLDER}")`,
          ``,
          `const data = await ${ep.sdk}()`,
        ].join("\n")
      }
      // No SDK surface for admin endpoints — an honest fetch snippet.
      return [
        `const res = await fetch("${url}", {`,
        `  method: "${ep.method}",`,
        `  headers: { "${headerName}": "${headerValue}" },`,
        ...(bodyJson
          ? [`  body: JSON.stringify(${bodyJson.replace(/\n/g, "\n  ")}),`]
          : []),
        `})`,
        `const data = await res.json()`,
      ].join("\n")
    }
    case "python": {
      const lines = [`import requests`, ``, `res = requests.${ep.method.toLowerCase()}(`, `    "${url}",`]
      lines.push(`    headers={"${headerName}": "${headerValue}"},`)
      if (ep.body) lines.push(`    json=${pyLiteral(ep.body)},`)
      lines.push(`)`, `data = res.json()`)
      return lines.join("\n")
    }
    case "go": {
      const bodyVar = ep.body
        ? `bytes.NewBufferString(\`${JSON.stringify(ep.body)}\`)`
        : "nil"
      const imports = ep.body
        ? `import (\n\t"bytes"\n\t"net/http"\n)`
        : `import "net/http"`
      return [
        imports,
        ``,
        `req, _ := http.NewRequest("${ep.method}", "${url}", ${bodyVar})`,
        `req.Header.Set("${headerName}", "${headerValue}")`,
        ...(ep.body ? [`req.Header.Set("Content-Type", "application/json")`] : []),
        `res, _ := http.DefaultClient.Do(req)`,
        `defer res.Body.Close()`,
      ].join("\n")
    }
  }
}

/** Minimal JSON → Python literal (dict/list/str/num/bool) for examples. */
function pyLiteral(value: unknown, indent = 4): string {
  const pad = " ".repeat(indent)
  const padEnd = " ".repeat(indent - 4)
  if (Array.isArray(value)) {
    return `[${value.map((v) => pyLiteral(v, indent)).join(", ")}]`
  }
  if (value && typeof value === "object") {
    const entries = Object.entries(value as Record<string, unknown>)
    return `{\n${entries
      .map(([k, v]) => `${pad}"${k}": ${pyLiteral(v, indent + 4)}`)
      .join(",\n")}\n${padEnd}}`
  }
  return JSON.stringify(value)
}
