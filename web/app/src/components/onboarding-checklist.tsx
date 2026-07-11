"use client"

// "Get started" card on the org overview (and /dashboard): four steps
// derived from real data — first email sent, a verified domain, a
// webhook, a second team member. Dismissable per org (localStorage);
// disappears on its own at 4/4.

import { useCallback, useState, useSyncExternalStore } from "react"
import Link from "next/link"
import { useQueries, useQuery } from "@tanstack/react-query"
import {
  ChevronDownIcon,
  ChevronRightIcon,
  CircleCheckIcon,
  CircleIcon,
  XIcon,
} from "lucide-react"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { CopyButton } from "@/components/shared"
import { adminApi } from "@/lib/api"
import { serverTotalMessages } from "@/lib/api-extras"

const DISMISS_PREFIX = "camelmailer.onboarding.dismissed."
const DISMISS_EVENT = "camelmailer:onboarding-dismissed"

function subscribeDismissed(callback: () => void): () => void {
  window.addEventListener("storage", callback)
  window.addEventListener(DISMISS_EVENT, callback)
  return () => {
    window.removeEventListener("storage", callback)
    window.removeEventListener(DISMISS_EVENT, callback)
  }
}

function curlSnippet(origin: string): string {
  return `curl ${origin || "https://your-camelmailer-host"}/api/v2/server/messages \\
  -H "X-Server-API-Key: $CAMELMAILER_API_KEY" \\
  -H "Content-Type: application/json" \\
  -d '{
    "from": "you@yourdomain.com",
    "to": ["you@example.com"],
    "subject": "Hello from CamelMailer",
    "text_body": "It works!"
  }'`
}

export function OnboardingChecklist({ org }: { org: string }) {
  // Dismissal lives in localStorage; the server snapshot says
  // "dismissed", so prerender/hydration render nothing and the real
  // card appears only on the client — no hydration mismatch.
  const dismissed = useSyncExternalStore(
    subscribeDismissed,
    useCallback(() => localStorage.getItem(DISMISS_PREFIX + org) === "1", [org]),
    () => true,
  )
  const [expanded, setExpanded] = useState(false)

  const servers = useQuery({
    queryKey: ["servers", org],
    queryFn: () => adminApi.servers(org).list(),
  })
  const serverList = servers.data?.servers ?? []
  const first = serverList[0]

  const members = useQuery({
    queryKey: ["members", org],
    queryFn: () => adminApi.members(org).list(),
    retry: false,
  })

  const stats = useQuery({
    queryKey: ["onboarding-stats", org, first?.permalink],
    queryFn: () => serverTotalMessages(org, first!.permalink),
    enabled: !!first,
    retry: false,
  })

  const domainQueries = useQueries({
    queries: serverList.map((server) => ({
      queryKey: ["domains", org, server.permalink],
      queryFn: () => adminApi.domains(org, server.permalink).list(),
      retry: false,
    })),
  })
  const webhookQueries = useQueries({
    queries: serverList.map((server) => ({
      queryKey: ["webhooks", org, server.permalink],
      queryFn: () => adminApi.webhooks(org, server.permalink).list(),
      retry: false,
    })),
  })

  const sentDone = (stats.data ?? 0) > 0
  const domainDone = domainQueries.some((q) =>
    q.data?.domains.some((domain) => domain.verified),
  )
  const webhookDone = webhookQueries.some((q) => (q.data?.webhooks.length ?? 0) > 0)
  const teamDone = (members.data?.members.length ?? 0) >= 2

  const serverBase = first ? `/orgs/${org}/servers/${first.permalink}` : `/orgs/${org}`
  const steps: { label: string; done: boolean; href: string; expandable?: boolean }[] = [
    {
      label: "Send your first email",
      done: sentDone,
      href: first ? `${serverBase}/messaging` : `/orgs/${org}`,
      expandable: true,
    },
    { label: "Verify a domain", done: domainDone, href: `${serverBase}/domains` },
    { label: "Set up a webhook", done: webhookDone, href: `${serverBase}/webhooks` },
    { label: "Invite your team", done: teamDone, href: `/orgs/${org}/invitations` },
  ]
  const doneCount = steps.filter((step) => step.done).length

  if (dismissed || doneCount === 4 || !servers.isSuccess) return null

  const snippet = curlSnippet(window.location.origin)

  return (
    <Card className="mb-6">
      <CardHeader className="flex flex-row items-center justify-between space-y-0">
        <CardTitle className="flex items-center gap-2 text-base">
          Get started
          <Badge variant="secondary" className="tabular-nums">
            {doneCount}/4
          </Badge>
        </CardTitle>
        <Button
          variant="ghost"
          size="icon"
          className="size-7"
          onClick={() => {
            localStorage.setItem(DISMISS_PREFIX + org, "1")
            window.dispatchEvent(new Event(DISMISS_EVENT))
          }}
        >
          <XIcon className="size-4" />
          <span className="sr-only">Dismiss</span>
        </Button>
      </CardHeader>
      <CardContent className="grid gap-1">
        {steps.map((step) => (
          <div key={step.label}>
            <div className="flex items-center gap-3 rounded-md px-2 py-1.5 hover:bg-accent/50">
              {step.done ? (
                <CircleCheckIcon className="size-4 shrink-0 text-green-600 dark:text-green-500" />
              ) : (
                <CircleIcon className="size-4 shrink-0 text-muted-foreground" />
              )}
              {step.expandable && !step.done ? (
                <button
                  className="flex flex-1 items-center gap-1 text-left text-sm"
                  onClick={() => setExpanded((value) => !value)}
                >
                  {step.label}
                  {expanded ? (
                    <ChevronDownIcon className="size-3.5 text-muted-foreground" />
                  ) : (
                    <ChevronRightIcon className="size-3.5 text-muted-foreground" />
                  )}
                </button>
              ) : (
                <Link
                  href={step.href}
                  className={`flex-1 text-sm ${step.done ? "text-muted-foreground line-through" : ""}`}
                >
                  {step.label}
                </Link>
              )}
              {!step.done && (
                <Button asChild variant="ghost" size="sm" className="h-7 text-xs">
                  <Link href={step.href}>Open</Link>
                </Button>
              )}
            </div>
            {step.expandable && !step.done && expanded && (
              <div className="mb-2 ml-7 mr-2 grid gap-2 rounded-md border bg-muted/40 p-3">
                <div className="flex items-start gap-2">
                  <pre className="min-w-0 flex-1 overflow-x-auto rounded bg-muted p-2 font-mono text-xs">
                    {snippet}
                  </pre>
                  <CopyButton value={snippet} />
                </div>
                <p className="text-xs text-muted-foreground">
                  Replace <code>$CAMELMAILER_API_KEY</code> with an API credential from{" "}
                  <Link href={`${serverBase}/credentials`} className="underline">
                    the Credentials page
                  </Link>
                  , and the from address with one of your verified domains or sender
                  addresses.
                </p>
              </div>
            )}
          </div>
        ))}
      </CardContent>
    </Card>
  )
}
