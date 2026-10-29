// Rendering provider-supplied UI (billing checkout, subscription, usage) and
// third-party widgets (the registration captcha). Pure helpers live here so
// they are testable without a DOM; the components are in
// components/embed-session.tsx and components/cap-widget.tsx.

import type { EmbedSession } from "@/lib/api"

/** A valid custom element name: lowercase, starts with a letter, contains a
 *  hyphen, no characters that could break out of a tag. Built-in elements
 *  never contain a hyphen, so this also keeps a provider from asking the
 *  dashboard to render a `script` or an `iframe`. */
export function isCustomElementName(name: string | null | undefined): name is string {
  return !!name && /^[a-z][a-z0-9._]*-[a-z0-9._-]*$/.test(name)
}

/** Only http(s) URLs are loaded or linked. */
export function isHttpUrl(url: string | null | undefined): url is string {
  if (!url) return false
  try {
    const parsed = new URL(url)
    return parsed.protocol === "https:" || parsed.protocol === "http:"
  } catch {
    return false
  }
}

/** Attribute names a provider may set on its element. Event handlers
 *  (`on…`) and anything outside a plain name pattern are dropped. */
export function safeAttributes(attributes: Record<string, string> | null | undefined) {
  const result: Record<string, string> = {}
  for (const [name, value] of Object.entries(attributes ?? {})) {
    if (/^[a-z][a-z0-9-]*$/.test(name) && !name.startsWith("on") && name !== "token") {
      result[name] = String(value)
    }
  }
  return result
}

export type EmbedElement = { element: string; attributes: Record<string, string> }

export type EmbedPlan =
  | {
      kind: "element"
      element: string
      scriptUrl: string
      attributes: Record<string, string>
      /** Further elements rendered after the first, same script and token. */
      companions: EmbedElement[]
      fallbackUrl: string | null
    }
  | { kind: "link"; url: string }
  | { kind: "none" }

/** How to show a session: its component when it names a valid one with a
 *  loadable script, else its link, else nothing. */
export function embedPlan(session: EmbedSession | null | undefined): EmbedPlan {
  if (!session) return { kind: "none" }
  const fallbackUrl = isHttpUrl(session.url) ? session.url : null
  if (isCustomElementName(session.element) && isHttpUrl(session.script_url)) {
    const withToken = (attributes: Record<string, string> | null | undefined) => {
      const safe = safeAttributes(attributes)
      if (session.token) safe.token = session.token
      return safe
    }
    const companions = (session.companions ?? [])
      .filter((companion) => isCustomElementName(companion?.element))
      .map((companion) => ({
        element: companion.element,
        attributes: withToken(companion.attributes),
      }))
    return {
      kind: "element",
      element: session.element,
      scriptUrl: session.script_url,
      attributes: withToken(session.attributes),
      companions,
      fallbackUrl,
    }
  }
  if (fallbackUrl) return { kind: "link", url: fallbackUrl }
  return { kind: "none" }
}

const loading = new Map<string, Promise<void>>()

/** Load a script once per URL. `module` loads it as an ES module (the Cap
 *  widget ships as one). */
export function loadScript(src: string, options: { module?: boolean } = {}): Promise<void> {
  const existing = loading.get(src)
  if (existing) return existing
  const promise = new Promise<void>((resolve, reject) => {
    const script = document.createElement("script")
    script.src = src
    script.async = true
    if (options.module) script.type = "module"
    script.onload = () => resolve()
    script.onerror = () => {
      loading.delete(src)
      script.remove()
      reject(new Error(`could not load ${src}`))
    }
    document.head.appendChild(script)
  })
  loading.set(src, promise)
  return promise
}
