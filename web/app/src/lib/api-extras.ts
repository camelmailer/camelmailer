// Client helpers layered on top of the typed API client (`api.ts`).
//
// Deliberately a separate file: new convenience functions land here so
// parallel feature branches do not contend over api.ts.

import { adminApi, serverApi, type Server } from "@/lib/api"

// ------------------------------------------------- last active org

const LAST_ORG_KEY = "camelmailer.last_org"
const LAST_ORG_EVENT = "camelmailer:last-org"

/// The organization the user last worked in (permalink), if any.
export function getLastActiveOrg(): string | null {
  if (typeof window === "undefined") return null
  return localStorage.getItem(LAST_ORG_KEY)
}

export function setLastActiveOrg(permalink: string) {
  if (typeof window === "undefined") return
  if (localStorage.getItem(LAST_ORG_KEY) === permalink) return
  localStorage.setItem(LAST_ORG_KEY, permalink)
  window.dispatchEvent(new Event(LAST_ORG_EVENT))
}

/// For useSyncExternalStore: re-render when the last active org changes
/// (this tab via the event above, other tabs via `storage`).
export function subscribeLastActiveOrg(callback: () => void): () => void {
  window.addEventListener("storage", callback)
  window.addEventListener(LAST_ORG_EVENT, callback)
  return () => {
    window.removeEventListener("storage", callback)
    window.removeEventListener(LAST_ORG_EVENT, callback)
  }
}

// ------------------------------------------------- server colors

/// The dot color of a server: its stored color when set, otherwise a
/// deterministic hue derived from the name (stable across renders and
/// sessions, no config needed).
export function serverDotColor(server: Pick<Server, "name" | "color">): string {
  if (server.color) return server.color
  let hash = 0
  for (const ch of server.name) hash = (hash * 31 + (ch.codePointAt(0) ?? 0)) >>> 0
  return `hsl(${hash % 360} 65% 50%)`
}

/// Up-to-two-letter initials for an organization avatar badge.
export function orgInitials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean)
  return (
    words
      .slice(0, 2)
      .map((word) => word[0]!.toUpperCase())
      .join("") || "?"
  )
}

// ------------------------------------------------- onboarding data

/// The first usable API credential key of a server, or null (no
/// credential yet / none active).
export async function firstApiCredentialKey(
  org: string,
  server: string,
): Promise<string | null> {
  const { credentials } = await adminApi.credentials(org, server).list()
  return credentials.find((c) => c.type === "API" && !c.hold)?.key ?? null
}

/// Total message count of a server via its own messaging API, or null
/// when unavailable (no API credential). Drives the onboarding
/// checklist's "send your first email" step.
export async function serverTotalMessages(
  org: string,
  server: string,
): Promise<number | null> {
  const key = await firstApiCredentialKey(org, server)
  if (!key) return null
  const { stats } = await serverApi(key).stats()
  return stats.total
}
