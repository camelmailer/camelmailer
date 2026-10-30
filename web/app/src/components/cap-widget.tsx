"use client"

// The Cap captcha widget (https://trycap.dev) for the sign-up form. The
// script defines <cap-widget>; once solved it fires a `solve` event whose
// detail carries the token, which the form sends as `captcha_token`.

import { createElement, useEffect, useState } from "react"
import { loadScript } from "@/lib/embed"

// Pinned so a widget release cannot change the sign-up page unannounced.
export const CAP_WIDGET_SCRIPT =
  "https://cdn.jsdelivr.net/npm/@cap.js/widget@0.1.58/cap.min.js"

export function CapWidget({
  endpoint,
  onToken,
}: {
  endpoint: string
  onToken: (token: string | null) => void
}) {
  // A callback ref into state, so the listener effect re-runs once the
  // element exists.
  const [element, setElement] = useState<HTMLElement | null>(null)
  const [failed, setFailed] = useState(false)

  useEffect(() => {
    loadScript(CAP_WIDGET_SCRIPT, { module: true }).catch(() => setFailed(true))
  }, [])

  useEffect(() => {
    if (!element) return
    const solved = (event: Event) => {
      const token = (event as CustomEvent<{ token?: string }>).detail?.token
      onToken(token ?? null)
    }
    const cleared = () => onToken(null)
    element.addEventListener("solve", solved)
    element.addEventListener("reset", cleared)
    element.addEventListener("error", cleared)
    return () => {
      element.removeEventListener("solve", solved)
      element.removeEventListener("reset", cleared)
      element.removeEventListener("error", cleared)
    }
  }, [element, onToken])

  if (failed) {
    return (
      <p className="text-sm text-destructive">
        The captcha could not be loaded. Check your connection and reload the page.
      </p>
    )
  }
  return createElement("cap-widget", {
    ref: setElement,
    // Full width like the inputs around it; the widget reads its width from
    // this custom property (default 260px).
    className: "block w-full",
    style: { "--cap-widget-width": "100%" },
    "data-cap-api-endpoint": endpoint,
    // The widget follows the browser language otherwise; the dashboard is English.
    "data-cap-lang": "en",
    // The form submits the token itself; no hidden field is needed.
    "data-cap-hidden-field-name": "cap-token",
  })
}
