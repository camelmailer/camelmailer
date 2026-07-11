"use client"

// Phase-P3 ("tools") clients and helpers, layered on top of the typed
// client in api.ts.
//
// Deliberately a separate file (same convention as api-extras / api-p1 /
// api-p2): new helpers land here so parallel feature branches do not
// contend over api.ts. Covers the template library + client-side render
// for live previews, webhook event metadata + sample payloads, credential
// "last used" formatting + masking + SMTP connection facts, and the
// per-language send snippets of the Setup tab.

import type { Credential, Domain } from "@/lib/api"
import type { PillTone } from "@/components/status-pill"
import libraryData from "@/lib/template-library.json"

// ---------------------------------------------------------- constants

/** The risk-free test recipient every message can be sent to. */
export const BLACKHOLE_ADDRESS = "test@blackhole.camelmailer.com"

/** The official SDK documentation. */
export const SDK_DOCS_URL = "https://camelmailer.com/docs/sdks"

/** "Switching from …" migration guides (short callouts on the Setup tab). */
export const MIGRATION_GUIDES = [
  { from: "Postal", href: "https://camelmailer.com/docs/migrate/postal" },
  { from: "Resend", href: "https://camelmailer.com/docs/migrate/resend" },
  { from: "Amazon SES", href: "https://camelmailer.com/docs/migrate/ses" },
] as const

// ------------------------------------------------------ template library

/** One of the 20 bundled transactional templates (source: `templates/` in
 *  the repo, snapshotted into template-library.json). `name` doubles as
 *  the permalink the import creates. */
export type LibraryTemplate = {
  name: string
  permalink: string
  description: string
  variables: string[]
  subject: string
  html_body: string
  text_body: string
}

export const TEMPLATE_LIBRARY = libraryData as LibraryTemplate[]

// --------------------------------------------------- client-side render

/** Resolve a dotted path (`user.name`) against a model object. */
function lookup(model: unknown, path: string): unknown {
  if (path === ".") return model
  let current: unknown = model
  for (const part of path.split(".")) {
    if (current && typeof current === "object" && part in (current as Record<string, unknown>)) {
      current = (current as Record<string, unknown>)[part]
    } else {
      return undefined
    }
  }
  return current
}

function truthy(value: unknown): boolean {
  if (Array.isArray(value)) return value.length > 0
  return Boolean(value)
}

function stringify(value: unknown): string {
  if (value == null) return ""
  if (typeof value === "object") return JSON.stringify(value)
  return String(value)
}

/** Render the CamelMailer Mustache subset — `{{ var }}` (dotted paths),
 *  `{{{ var }}}` (kept raw), `{{#section}}…{{/section}}` (truthy / list
 *  iteration) and `{{^section}}…{{/section}}` (inverted) — entirely in the
 *  browser, so the split editor previews *unsaved* edits live. Unknown
 *  variables render empty, mirroring the backend renderer. Best-effort:
 *  never throws. */
export function renderMustache(template: string, model: unknown): string {
  const section = /\{\{([#^])\s*([\w.]+)\s*\}\}([\s\S]*?)\{\{\/\s*\2\s*\}\}/g
  // Sections first (repeatedly, to resolve nesting), then variables.
  let out = template
  let guard = 0
  while (section.test(out) && guard < 20) {
    section.lastIndex = 0
    out = out.replace(section, (_all, kind: string, name: string, inner: string) => {
      const value = lookup(model, name)
      if (kind === "^") return truthy(value) ? "" : renderMustache(inner, model)
      if (Array.isArray(value)) {
        return value.map((item) => renderMustache(inner, item)).join("")
      }
      return truthy(value) ? renderMustache(inner, value === true ? model : value) : ""
    })
    guard += 1
  }
  return out
    .replace(/\{\{\{\s*([\w.]+)\s*\}\}\}/g, (_all, name: string) => stringify(lookup(model, name)))
    .replace(/\{\{\s*([\w.]+)\s*\}\}/g, (_all, name: string) => stringify(lookup(model, name)))
}

/** Collect the `{{ variable }}` names referenced by a template body, so the
 *  editor can pre-seed a sensible test model. */
export function extractVariables(...parts: (string | null | undefined)[]): string[] {
  const found = new Set<string>()
  const re = /\{\{[#^/]?\s*([\w.]+)\s*\}\}/g
  for (const part of parts) {
    if (!part) continue
    let match: RegExpExecArray | null
    while ((match = re.exec(part))) {
      found.add(match[1].split(".")[0])
    }
  }
  return [...found]
}

/** A plausible sample value for a variable name (used to seed the test
 *  model and the gallery thumbnails). */
export function sampleValue(name: string): string {
  const n = name.toLowerCase()
  if (n.includes("url") || n.includes("link")) return "https://example.com/action"
  if (n.includes("email")) return "you@example.com"
  if (n.includes("name")) return "Ada Lovelace"
  if (n.includes("product") || n.includes("company") || n.includes("app")) return "Acme"
  if (n.includes("code")) return "123456"
  if (n.includes("amount") || n.includes("total") || n.includes("price")) return "$42.00"
  if (n.includes("date") || n.includes("expires")) return "in 2 hours"
  return name
}

/** Build a `{ variable: sample }` model covering every referenced name. */
export function sampleModel(...parts: (string | null | undefined)[]): Record<string, string> {
  return Object.fromEntries(extractVariables(...parts).map((name) => [name, sampleValue(name)]))
}

// ------------------------------------------------------ webhook events

/** Display metadata per webhook event: the lifecycle tone (green sent,
 *  amber delayed/held, red failed) and a one-line explanation. Keyed by
 *  the real WEBHOOK_EVENTS names. */
export const WEBHOOK_EVENT_META: Record<
  string,
  { tone: PillTone; label: string; description: string }
> = {
  MessageSent: {
    tone: "green",
    label: "Sent",
    description: "A message was accepted by the recipient's mail server.",
  },
  MessageDelayed: {
    tone: "amber",
    label: "Delayed",
    description: "Delivery was deferred and will be retried.",
  },
  MessageDeliveryFailed: {
    tone: "red",
    label: "Failed",
    description: "Delivery failed permanently (a hard bounce or rejection).",
  },
  MessageHeld: {
    tone: "amber",
    label: "Held",
    description: "The message was held for manual review before sending.",
  },
}

const SAMPLE_DETAILS: Record<string, string> = {
  MessageSent: "Message for recipient@example.com accepted by mx.example.com",
  MessageDelayed: "421 4.7.0 try again later (attempt 2 of 18)",
  MessageDeliveryFailed: "550 5.1.1 recipient address rejected: user unknown",
  MessageHeld: "Message held for manual review (spam score above threshold)",
}

/** The example JSON payload for an event — mirrors the backend's
 *  `sample_payload`, so what the editor shows matches what arrives. */
export function webhookSamplePayload(event: string): string {
  return JSON.stringify(
    {
      event,
      timestamp: Math.floor(Date.now() / 1000),
      uuid: "<generated per delivery>",
      test: true,
      payload: {
        message: {
          id: 1234,
          token: "AbCdEf123456",
          rcpt_to: "recipient@example.com",
          mail_from: "sender@yourdomain.com",
          scope: "outgoing",
          bounce: false,
        },
        details: SAMPLE_DETAILS[event] ?? "Test delivery",
      },
    },
    null,
    2,
  )
}

// ----------------------------------------------------------- credentials

/** Credentials carry `last_used_at` (see openapi), not yet in the shared
 *  `Credential` type — widen it here rather than touch api.ts. */
export type CredentialWithUsage = Credential & { last_used_at?: string | null }

/** Mask a credential key to a leading + trailing fragment, keeping any
 *  `cm_` prefix visible so the key type stays recognizable. */
export function maskKey(key: string): string {
  const prefixMatch = key.match(/^([a-z]+_)/)
  const prefix = prefixMatch ? prefixMatch[1] : ""
  const rest = key.slice(prefix.length)
  const tail = rest.slice(-4)
  return `${prefix}${"•".repeat(Math.min(8, Math.max(4, rest.length - 4)))}${tail}`
}

// ---------------------------------------------------------- SMTP facts

/** The SMTP submission ports (Postal-compatible). 465 is offered as the
 *  implicit-TLS alternative. */
export const SMTP_PORTS = [
  { port: "587", note: "STARTTLS (recommended)" },
  { port: "25", note: "STARTTLS, opportunistic" },
  { port: "465", note: "implicit TLS" },
]

/** The SMTP AUTH username of a server: `<org>/<server>` (the backend
 *  splits on `/` or `_`). The password is any SMTP-type credential key. */
export function smtpUsername(org: string, server: string): string {
  return `${org}/${server}`
}

/** Best-effort SMTP hostname: parsed from a domain's SPF record (the
 *  backend embeds the real `smtp_hostname` there as `a:<host>`), falling
 *  back to the platform host the dashboard is served from. */
export function deriveSmtpHost(domains: Domain[] | undefined, fallbackHost: string): string {
  for (const domain of domains ?? []) {
    const spf = domain.spf_record?.value ?? ""
    const match = spf.match(/\ba:([A-Za-z0-9.-]+)/)
    if (match) return match[1]
  }
  return fallbackHost.replace(/^app\./, "smtp.")
}

// --------------------------------------------------------- send snippets

export type SnippetLang = {
  id: string
  label: string
  /** Highlight hint / file extension for the mono block. */
  syntax: string
}

export const SNIPPET_LANGS: SnippetLang[] = [
  { id: "curl", label: "curl", syntax: "bash" },
  { id: "node", label: "Node.js", syntax: "javascript" },
  { id: "python", label: "Python", syntax: "python" },
  { id: "php", label: "PHP", syntax: "php" },
  { id: "go", label: "Go", syntax: "go" },
  { id: "ruby", label: "Ruby", syntax: "ruby" },
  { id: "rails", label: "Rails", syntax: "ruby" },
  { id: "smtp", label: "SMTP", syntax: "text" },
]

export type SnippetContext = {
  /** e.g. https://mail.acme.com — no trailing slash. */
  origin: string
  apiKey: string
  from: string
  to: string
  /** SMTP-only extras. */
  smtpHost: string
  smtpUser: string
}

const SUBJECT = "Hello from CamelMailer"
const HTMLBODY = "<h1>It works!</h1><p>Your first message is on its way.</p>"

/** A copy-paste-ready send snippet in the given language, with the real
 *  server endpoint + API key already filled in (zero-edit onboarding). */
export function sendSnippet(lang: string, ctx: SnippetContext): string {
  const url = `${ctx.origin}/api/v2/server/messages`
  switch (lang) {
    case "curl":
      return `curl -X POST "${url}" \\
  -H "X-Server-API-Key: ${ctx.apiKey}" \\
  -H "Content-Type: application/json" \\
  -d '{
    "from": "${ctx.from}",
    "to": ["${ctx.to}"],
    "subject": "${SUBJECT}",
    "html_body": "${HTMLBODY}"
  }'`
    case "node":
      return `const res = await fetch("${url}", {
  method: "POST",
  headers: {
    "X-Server-API-Key": "${ctx.apiKey}",
    "Content-Type": "application/json",
  },
  body: JSON.stringify({
    from: "${ctx.from}",
    to: ["${ctx.to}"],
    subject: "${SUBJECT}",
    html_body: "${HTMLBODY}",
  }),
})
console.log(await res.json())`
    case "python":
      return `import requests

res = requests.post(
    "${url}",
    headers={"X-Server-API-Key": "${ctx.apiKey}"},
    json={
        "from": "${ctx.from}",
        "to": ["${ctx.to}"],
        "subject": "${SUBJECT}",
        "html_body": "${HTMLBODY}",
    },
)
print(res.json())`
    case "php":
      return `<?php
$ch = curl_init("${url}");
curl_setopt_array($ch, [
    CURLOPT_POST => true,
    CURLOPT_RETURNTRANSFER => true,
    CURLOPT_HTTPHEADER => [
        "X-Server-API-Key: ${ctx.apiKey}",
        "Content-Type: application/json",
    ],
    CURLOPT_POSTFIELDS => json_encode([
        "from" => "${ctx.from}",
        "to" => ["${ctx.to}"],
        "subject" => "${SUBJECT}",
        "html_body" => "${HTMLBODY}",
    ]),
]);
echo curl_exec($ch);`
    case "go":
      return `package main

import (
	"bytes"
	"net/http"
)

func main() {
	body := []byte(\`{"from":"${ctx.from}","to":["${ctx.to}"],"subject":"${SUBJECT}","html_body":"${HTMLBODY}"}\`)
	req, _ := http.NewRequest("POST", "${url}", bytes.NewReader(body))
	req.Header.Set("X-Server-API-Key", "${ctx.apiKey}")
	req.Header.Set("Content-Type", "application/json")
	http.DefaultClient.Do(req)
}`
    case "ruby":
      return `require "net/http"
require "json"

uri = URI("${url}")
res = Net::HTTP.post(
  uri,
  { from: "${ctx.from}", to: ["${ctx.to}"], subject: "${SUBJECT}", html_body: "${HTMLBODY}" }.to_json,
  "X-Server-API-Key" => "${ctx.apiKey}",
  "Content-Type" => "application/json",
)
puts res.body`
    case "rails":
      return `# config/environments/production.rb
config.action_mailer.delivery_method = :smtp
config.action_mailer.smtp_settings = {
  address:              "${ctx.smtpHost}",
  port:                 587,
  user_name:            "${ctx.smtpUser}",
  password:             "${ctx.apiKey}",  # an SMTP credential key
  authentication:       :plain,
  enable_starttls_auto: true,
}
# then deliver as usual: UserMailer.welcome(user).deliver_later`
    case "smtp":
      return `Host:      ${ctx.smtpHost}
Port:      587  (STARTTLS)  ·  465 (implicit TLS)  ·  25
Username:  ${ctx.smtpUser}
Password:  ${ctx.apiKey}   # an SMTP credential key
From:      ${ctx.from}
To:        ${ctx.to}`
    default:
      return ""
  }
}
