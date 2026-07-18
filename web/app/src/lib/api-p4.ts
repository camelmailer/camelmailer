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

export type CodeLang =
  | "curl"
  | "node"
  | "python"
  | "php"
  | "ruby"
  | "go"
  | "java"
  | "csharp"
  | "rust"

export const CODE_LANGS: { value: CodeLang; label: string }[] = [
  { value: "curl", label: "cURL" },
  { value: "node", label: "Node" },
  { value: "python", label: "Python" },
  { value: "php", label: "PHP" },
  { value: "ruby", label: "Ruby" },
  { value: "go", label: "Go" },
  { value: "java", label: "Java" },
  { value: "csharp", label: ".NET" },
  { value: "rust", label: "Rust" },
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

/** Context resolved from the active route + a masked key placeholder. The
 *  resource identifiers come from the dynamic route segments so detail-page
 *  snippets target exactly the record on screen. */
export type CodeContext = {
  baseUrl: string
  org: string
  server: string
  permalink?: string
  id?: string
  name?: string
  email?: string
  /** Keep `:id`/`:permalink`/… placeholders instead of the on-screen record. */
  generic?: boolean
}

const ADMIN = "/api/v2/admin/organizations/:org/servers/:server"

/** The static catalog, keyed by section id — one entry per UI surface, so the
 *  panel shows exactly the call that produces what is on screen. Mirrors
 *  public/openapi.yaml. */
export function endpointCatalog(): Record<string, ApiEndpoint> {
  return {
    // ---- server dashboard ----
    stats: {
      id: "stats",
      label: "Server statistics",
      description: "Aggregate counters: sent, bounced (by class), opens and clicks.",
      method: "GET",
      path: "/api/v2/server/stats",
      auth: "server",
    },
    // ---- messaging ----
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
    },
    messages: {
      id: "messages",
      label: "List messages",
      description: "Page through delivered, held and bounced mail with filters.",
      method: "GET",
      path: "/api/v2/server/messages?scope=outgoing&per_page=25",
      auth: "server",
    },
    message: {
      id: "message",
      label: "Show a message",
      description: "One message with its status, headers and delivery attempts.",
      method: "GET",
      path: "/api/v2/server/messages/:id",
      auth: "server",
    },
    // ---- recipients (derived from messages) ----
    recipients: {
      id: "recipients",
      label: "List messages",
      description: "Recipients are derived from the server's outgoing messages.",
      method: "GET",
      path: "/api/v2/server/messages?scope=outgoing&per_page=100",
      auth: "server",
    },
    recipient: {
      id: "recipient",
      label: "List a recipient's messages",
      description: "The outgoing messages addressed to one recipient.",
      method: "GET",
      path: "/api/v2/server/messages?scope=outgoing&recipient=:email",
      auth: "server",
    },
    // ---- streams ----
    streams: {
      id: "streams",
      label: "List message streams",
      description: "The server's transactional, broadcast and inbound streams.",
      method: "GET",
      path: "/api/v2/server/streams",
      auth: "server",
    },
    stream: {
      id: "stream",
      label: "List a stream's subscribers",
      description: "Opt-in subscribers of this stream (broadcast streams).",
      method: "GET",
      path: "/api/v2/server/streams/:permalink/subscribers",
      auth: "server",
    },
    // ---- campaigns ----
    campaigns: {
      id: "campaigns",
      label: "List campaigns",
      description: "Broadcast campaigns across the server's streams, newest first.",
      method: "GET",
      path: "/api/v2/server/campaigns",
      auth: "server",
    },
    campaign: {
      id: "campaign",
      label: "Show a campaign",
      description: "One campaign with its audience, status and send counters.",
      method: "GET",
      path: "/api/v2/server/campaigns/:id",
      auth: "server",
    },
    "campaign-new": {
      id: "campaign-new",
      label: "Create a campaign",
      description: "Plan a broadcast to a stream's subscribers (draft or scheduled).",
      method: "POST",
      path: "/api/v2/server/campaigns",
      auth: "server",
      body: {
        stream: "product-news",
        name: "August roundup",
        from: "news@acme.com",
        subject: "What shipped in August",
        html_body: "<h1>Hello</h1>",
      },
    },
    // ---- templates ----
    templates: {
      id: "templates",
      label: "List templates",
      description: "The server's transactional templates (draft + published).",
      method: "GET",
      path: "/api/v2/server/templates",
      auth: "server",
    },
    template: {
      id: "template",
      label: "Show a template",
      description: "One template's subject, HTML/text bodies and layout.",
      method: "GET",
      path: "/api/v2/server/templates/:permalink",
      auth: "server",
    },
    "template-new": {
      id: "template-new",
      label: "Create a template",
      description: "Add a Mustache-style template rendered per send.",
      method: "POST",
      path: "/api/v2/server/templates",
      auth: "server",
      body: {
        name: "welcome",
        subject: "Welcome, {{ name }}",
        html_body: "<h1>Welcome, {{ name }}</h1>",
        text_body: "Welcome, {{ name }}",
      },
    },
    // ---- layouts ----
    layouts: {
      id: "layouts",
      label: "List layouts",
      description: "Reusable shells (logo, header, footer) that wrap template bodies.",
      method: "GET",
      path: "/api/v2/server/layouts",
      auth: "server",
    },
    layout: {
      id: "layout",
      label: "Show a layout",
      description: "One layout's HTML/text wrapper.",
      method: "GET",
      path: "/api/v2/server/layouts/:permalink",
      auth: "server",
    },
    "layout-new": {
      id: "layout-new",
      label: "Create a layout",
      description: "A wrapper that embeds the body via {{{ content }}}.",
      method: "POST",
      path: "/api/v2/server/layouts",
      auth: "server",
      body: {
        name: "Brand",
        html_wrapper: "<div>{{{ content }}}</div>",
      },
    },
    // ---- deliverability + ops ----
    dmarc: {
      id: "dmarc",
      label: "DMARC summary",
      description: "Aggregate DMARC pass/fail rollup from ingested reports.",
      method: "GET",
      path: "/api/v2/server/dmarc/summary",
      auth: "server",
    },
    queue: {
      id: "queue",
      label: "Queue depth",
      description: "Pending and retrying delivery counts for this server.",
      method: "GET",
      path: "/api/v2/server/stats/deliveries",
      auth: "server",
    },
    logs: {
      id: "logs",
      label: "API request log",
      description: "Recent API requests made against this server.",
      method: "GET",
      path: "/api/v2/server/logs",
      auth: "server",
    },
    // ---- management (admin-scoped) ----
    domains: {
      id: "domains",
      label: "List domains",
      description: "Sending domains with their DKIM / return-path / verification state.",
      method: "GET",
      path: `${ADMIN}/domains`,
      auth: "admin",
    },
    domain: {
      id: "domain",
      label: "Show a domain",
      description: "One domain's DKIM record, verification token and status.",
      method: "GET",
      path: `${ADMIN}/domains/:name`,
      auth: "admin",
    },
    credentials: {
      id: "credentials",
      label: "List credentials",
      description: "API and SMTP credentials for this server.",
      method: "GET",
      path: `${ADMIN}/credentials`,
      auth: "admin",
    },
    credential: {
      id: "credential",
      label: "Show a credential",
      description: "One API/SMTP credential.",
      method: "GET",
      path: `${ADMIN}/credentials/:id`,
      auth: "admin",
    },
    routes: {
      id: "routes",
      label: "List routes",
      description: "Inbound routes and their endpoints for this server.",
      method: "GET",
      path: `${ADMIN}/routes`,
      auth: "admin",
    },
    webhooks: {
      id: "webhooks",
      label: "List webhooks",
      description: "Delivery webhooks configured for this server.",
      method: "GET",
      path: `${ADMIN}/webhooks`,
      auth: "admin",
    },
    webhook: {
      id: "webhook",
      label: "Show a webhook",
      description: "One webhook's URL, events and signing settings.",
      method: "GET",
      path: `${ADMIN}/webhooks/:id`,
      auth: "admin",
    },
    senders: {
      id: "senders",
      label: "List sender addresses",
      description: "Verified single sender addresses for From-authorization.",
      method: "GET",
      path: `${ADMIN}/sender_addresses`,
      auth: "admin",
    },
    suppressions: {
      id: "suppressions",
      label: "List suppressions",
      description: "Suppressed recipients (bounces, complaints, unsubscribes).",
      method: "GET",
      path: `${ADMIN}/suppressions`,
      auth: "admin",
    },
    settings: {
      id: "settings",
      label: "Show server settings",
      description: "This server's configuration (mode, tracking, spam thresholds).",
      method: "GET",
      path: ADMIN,
      auth: "admin",
    },
  }
}

/** Which catalog section fits the current path (the UI-as-API twin). Detail
 *  routes (with a trailing id/permalink/name) map to the single-record call;
 *  `/new` maps to the create call; list roots to the list call. */
export function resolveApiSection(pathname: string): string {
  const hasChild = (base: string) => new RegExp(`/${base}/[^/]+`).test(pathname)
  const has = (base: string) => pathname.includes(`/${base}`)

  if (pathname.includes("/templates/new")) return "template-new"
  if (hasChild("templates")) return "template"
  if (has("templates")) return "templates"

  if (pathname.includes("/layouts/new")) return "layout-new"
  if (hasChild("layouts")) return "layout"
  if (has("layouts")) return "layouts"

  if (pathname.includes("/campaigns/new")) return "campaign-new"
  if (hasChild("campaigns")) return "campaign"
  if (has("campaigns")) return "campaigns"

  if (hasChild("streams")) return "stream"
  if (has("streams")) return "streams"

  if (hasChild("messaging")) return "message"
  if (has("messaging")) return "messages"

  if (hasChild("recipients")) return "recipient"
  if (has("recipients")) return "recipients"

  if (hasChild("domains")) return "domain"
  if (has("domains")) return "domains"

  if (hasChild("credentials")) return "credential"
  if (has("credentials")) return "credentials"

  if (hasChild("webhooks")) return "webhook"
  if (has("webhooks")) return "webhooks"

  if (has("routes")) return "routes"
  if (has("sender-addresses")) return "senders"
  if (has("suppressions")) return "suppressions"
  if (has("dmarc")) return "dmarc"
  if (has("queue")) return "queue"
  if (has("logs")) return "logs"
  if (has("settings")) return "settings"

  // The server root (no sub-area) shows the dashboard stats.
  return "stats"
}

/** Whether an endpoint targets a single record (its path has a resource id). */
export function endpointHasResource(ep: ApiEndpoint): boolean {
  return /:(id|permalink|name|email)\b/.test(ep.path)
}

/** Resolve `:org`/`:server` always; resolve the resource id too unless
 *  `ctx.generic` (then the `:id`/`:permalink`/… placeholders are kept, so the
 *  snippet reads as the reusable, generic call). */
export function resolvePath(ep: ApiEndpoint, ctx: CodeContext): string {
  let path = ep.path.replace(":org", ctx.org || "ORG").replace(":server", ctx.server || "SERVER")
  if (ctx.generic) return path
  path = path
    .replace(":permalink", ctx.permalink || ":permalink")
    .replace(":name", ctx.name ? encodeURIComponent(ctx.name) : ":name")
    .replace(":email", ctx.email ? encodeURIComponent(ctx.email) : ":email")
    .replace(":id", ctx.id || ":id")
  return path
}

export const KEY_PLACEHOLDER = "cm_your_api_key"

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

  const bodyCompact = ep.body ? JSON.stringify(ep.body) : null
  const ct = `"Content-Type: application/json"`

  switch (lang) {
    case "curl": {
      const lines = [`curl -X ${ep.method} "${url}" \\`, `  -H "${headerName}: ${headerValue}"`]
      if (bodyCompact) {
        lines[lines.length - 1] += ` \\`
        lines.push(`  -H ${ct} \\`)
        lines.push(`  -d '${bodyCompact}'`)
      }
      return lines.join("\n")
    }
    case "node": {
      return [
        `const res = await fetch("${url}", {`,
        `  method: "${ep.method}",`,
        `  headers: {`,
        `    "${headerName}": "${headerValue}",`,
        ...(bodyJson ? [`    "Content-Type": "application/json",`] : []),
        `  },`,
        ...(bodyJson ? [`  body: JSON.stringify(${bodyJson.replace(/\n/g, "\n  ")}),`] : []),
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
    case "php": {
      const lines = [
        `<?php`,
        `$ch = curl_init("${url}");`,
        `curl_setopt_array($ch, [`,
        `  CURLOPT_CUSTOMREQUEST => "${ep.method}",`,
        `  CURLOPT_RETURNTRANSFER => true,`,
        `  CURLOPT_HTTPHEADER => [`,
        `    "${headerName}: ${headerValue}",`,
        ...(bodyCompact ? [`    "Content-Type: application/json",`] : []),
        `  ],`,
        ...(bodyCompact ? [`  CURLOPT_POSTFIELDS => '${bodyCompact}',`] : []),
        `]);`,
        `$data = json_decode(curl_exec($ch), true);`,
      ]
      return lines.join("\n")
    }
    case "ruby": {
      return [
        `require "net/http"`,
        `require "json"`,
        ``,
        `uri = URI("${url}")`,
        `req = Net::HTTP::${ep.method.charAt(0) + ep.method.slice(1).toLowerCase()}.new(uri)`,
        `req["${headerName}"] = "${headerValue}"`,
        ...(bodyCompact ? [`req["Content-Type"] = "application/json"`, `req.body = ${JSON.stringify(bodyCompact)}`] : []),
        `res = Net::HTTP.start(uri.host, uri.port, use_ssl: uri.scheme == "https") { |http| http.request(req) }`,
        `data = JSON.parse(res.body)`,
      ].join("\n")
    }
    case "go": {
      const bodyVar = bodyCompact ? `strings.NewReader(\`${bodyCompact}\`)` : "nil"
      const imports = bodyCompact
        ? `import (\n\t"net/http"\n\t"strings"\n)`
        : `import "net/http"`
      return [
        imports,
        ``,
        `req, _ := http.NewRequest("${ep.method}", "${url}", ${bodyVar})`,
        `req.Header.Set("${headerName}", "${headerValue}")`,
        ...(bodyCompact ? [`req.Header.Set("Content-Type", "application/json")`] : []),
        `res, _ := http.DefaultClient.Do(req)`,
        `defer res.Body.Close()`,
      ].join("\n")
    }
    case "java": {
      const lines = [
        `HttpRequest req = HttpRequest.newBuilder()`,
        `    .uri(URI.create("${url}"))`,
        `    .header("${headerName}", "${headerValue}")`,
      ]
      if (bodyCompact) {
        lines.push(`    .header("Content-Type", "application/json")`)
        lines.push(`    .method("${ep.method}", HttpRequest.BodyPublishers.ofString(${JSON.stringify(bodyCompact)}))`)
      } else {
        lines.push(`    .method("${ep.method}", HttpRequest.BodyPublishers.noBody())`)
      }
      lines.push(`    .build();`)
      lines.push(`HttpResponse<String> res = HttpClient.newHttpClient()`)
      lines.push(`    .send(req, HttpResponse.BodyHandlers.ofString());`)
      return lines.join("\n")
    }
    case "csharp": {
      const lines = [
        `using var client = new HttpClient();`,
        `var req = new HttpRequestMessage(new HttpMethod("${ep.method}"), "${url}");`,
        `req.Headers.Add("${headerName}", "${headerValue}");`,
      ]
      if (bodyCompact) {
        lines.push(
          `req.Content = new StringContent(${JSON.stringify(bodyCompact)}, System.Text.Encoding.UTF8, "application/json");`,
        )
      }
      lines.push(`var res = await client.SendAsync(req);`)
      lines.push(`var data = await res.Content.ReadAsStringAsync();`)
      return lines.join("\n")
    }
    case "rust": {
      const lines = [
        `let client = reqwest::Client::new();`,
        `let res = client`,
        `    .request(reqwest::Method::${ep.method}, "${url}")`,
        `    .header("${headerName}", "${headerValue}")`,
      ]
      if (bodyCompact) {
        lines.push(`    .header("Content-Type", "application/json")`)
        lines.push(`    .body(r#"${bodyCompact}"#)`)
      }
      lines.push(`    .send()`, `    .await?;`)
      return lines.join("\n")
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
