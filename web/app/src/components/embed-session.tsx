"use client"

// Renders an EmbedSession from the billing provider: its custom element
// (loaded from the provider's script) with a short-lived token, or a plain
// link when the provider offers no component or its script fails to load.

import { createElement, useEffect, useState } from "react"
import { ExternalLinkIcon } from "lucide-react"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import type { EmbedSession } from "@/lib/api"
import { embedPlan, loadScript } from "@/lib/embed"

export function EmbedSessionView({
  session,
  linkLabel = "Open",
}: {
  session: EmbedSession | null | undefined
  linkLabel?: string
}) {
  const plan = embedPlan(session)
  const scriptUrl = plan.kind === "element" ? plan.scriptUrl : null
  // The state is keyed by the script URL, so a new session for another
  // script starts over without resetting state inside the effect.
  const [loaded, setLoaded] = useState<{ src: string; ok: boolean } | null>(null)

  useEffect(() => {
    if (!scriptUrl) return
    let active = true
    loadScript(scriptUrl).then(
      () => active && setLoaded({ src: scriptUrl, ok: true }),
      () => active && setLoaded({ src: scriptUrl, ok: false }),
    )
    return () => {
      active = false
    }
  }, [scriptUrl])

  if (plan.kind === "none") return null
  const link = (url: string) => (
    <Button asChild variant="outline">
      <a href={url} target="_blank" rel="noreferrer">
        {linkLabel} <ExternalLinkIcon className="size-4" />
      </a>
    </Button>
  )
  if (plan.kind === "link") return link(plan.url)

  const state = loaded?.src === plan.scriptUrl ? loaded : null
  if (!state) return <Skeleton className="h-24 w-full" />
  if (!state.ok) {
    return plan.fallbackUrl ? (
      link(plan.fallbackUrl)
    ) : (
      <p className="text-sm text-muted-foreground">
        The billing component could not be loaded. Please try again later.
      </p>
    )
  }
  return createElement(plan.element, plan.attributes)
}
