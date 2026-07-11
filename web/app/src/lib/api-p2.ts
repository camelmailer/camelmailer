"use client"

// Phase-P2 ("trust") API clients and helpers, layered on top of the
// typed client in api.ts.
//
// Deliberately a separate file (same convention as api-extras.ts): new
// convenience functions land here so parallel feature branches do not
// contend over api.ts.

import { useMemo } from "react"
import { useQuery } from "@tanstack/react-query"
import {
  adminApi,
  api,
  serverApi,
  type DnsRecord,
  type Domain,
  type Suppression,
} from "@/lib/api"

// -------------------------------------------------------- single domain

/// GET one sending domain (the list endpoint's fields, incl. the three
/// DNS records) — drives the domain-detail view.
export function getDomain(org: string, server: string, name: string) {
  return api.get<{ domain: Domain }>(
    `/api/v2/admin/organizations/${org}/servers/${server}/domains/${encodeURIComponent(name)}`,
  )
}

// ------------------------------------------- per-server messaging API

/// The per-server messaging API bound to the first usable API
/// credential of the server — the same rule MessagingShell and the
/// DMARC view apply. `api` stays null while the credentials load or
/// when the server has no active API credential.
export function useServerMessagingApi(org: string, server: string) {
  const credentials = useQuery({
    queryKey: ["credentials", org, server],
    queryFn: () => adminApi.credentials(org, server).list(),
  })
  const apiKey = useMemo(
    () =>
      credentials.data?.credentials.find(
        (credential) => credential.type === "API" && !credential.hold,
      )?.key ?? null,
    [credentials.data],
  )
  const sapi = useMemo(() => (apiKey ? serverApi(apiKey) : null), [apiKey])
  return { api: sapi, isLoading: credentials.isLoading }
}

// ---------------------------------------------------- recipient detail

/// App route of the recipient-detail view.
export function recipientHref(org: string, server: string, email: string): string {
  return `/orgs/${org}/servers/${server}/recipients/${encodeURIComponent(email)}`
}

// ------------------------------------------------- suppressions extras

/// `created_at` exists in the database schema but is not serialized by
/// every backend version — render it when the API starts carrying it.
export type SuppressionWithDate = Suppression & { created_at?: string | null }

const REASON_TEXT: Record<string, string> = {
  bounce: "Hard bounce",
  hard_bounce: "Hard bounce",
  hardfail: "Hard bounce",
  complaint: "Spam complaint",
  spam: "Marked as spam",
  unsubscribe: "Unsubscribed",
  manual: "Added manually",
}

/// Plain-language rendering of a suppression reason; free-text reasons
/// pass through unchanged, a missing reason reads "Added manually".
export function suppressionReasonText(suppression: Suppression): string {
  const reason = suppression.reason?.trim()
  if (!reason) return "Added manually"
  return REASON_TEXT[reason.toLowerCase().replace(/[\s-]/g, "_")] ?? reason
}

/// The current suppression list as CSV (compliance export, client-side).
export function suppressionsCsv(suppressions: SuppressionWithDate[]): string {
  const escape = (value: string) =>
    /[",\n]/.test(value) ? `"${value.replaceAll('"', '""')}"` : value
  const rows = [
    ["address", "type", "reason", "date_added"],
    ...suppressions.map((s) => [s.address, s.type, s.reason ?? "", s.created_at ?? ""]),
  ]
  return rows.map((row) => row.map(escape).join(",")).join("\n") + "\n"
}

/// Offer `content` as a file download (no server round-trip).
export function downloadFile(filename: string, content: string, type = "text/csv") {
  const url = URL.createObjectURL(new Blob([content], { type }))
  const anchor = document.createElement("a")
  anchor.href = url
  anchor.download = filename
  anchor.click()
  URL.revokeObjectURL(url)
}

// ---------------------------------------- DNS instructions to a teammate

/// The backend has no "email these DNS records" endpoint (checked
/// against openapi.yaml), so the delegation flow is a prefilled
/// `mailto:` draft — the operator's own mail client sends it. The body
/// carries every record in full (name + value untruncated).
export function dnsInstructionsMailto(
  domain: Domain,
  recipient: string,
  note?: string,
): string {
  const record = (label: string, r: DnsRecord | null) =>
    r ? `${label}\n  Type:  ${r.type}\n  Name:  ${r.name}\n  Value: ${r.value}\n` : ""
  const body = [
    `Hi,\n\ncould you publish these DNS records for ${domain.name}? They let our transactional mail authenticate (ownership, SPF and DKIM).\n`,
    record("1. Domain verification (TXT)", domain.verification_record),
    record("2. SPF (TXT)", domain.spf_record),
    record("3. DKIM (TXT)", domain.dkim_record),
    note?.trim() ? `${note.trim()}\n` : "",
    "Once the records are live, reply here and we'll verify the domain.\n\nThanks!",
  ]
    .filter(Boolean)
    .join("\n")
  const subject = `DNS records for ${domain.name}`
  return `mailto:${encodeURIComponent(recipient)}?subject=${encodeURIComponent(subject)}&body=${encodeURIComponent(body)}`
}
