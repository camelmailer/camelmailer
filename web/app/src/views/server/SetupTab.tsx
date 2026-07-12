"use client"

// Per-server Setup tab (masterplan §3): pick a language, get a runnable
// send snippet with THIS server's endpoint + first API credential already
// filled in (zero-edit onboarding), the Blackhole test address, links to
// the official SDKs and short "switching from …" migration callouts.

import { useMemo, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { ExternalLinkIcon, KeyRoundIcon } from "lucide-react"
import { CopyButton, PageHeader } from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
import { adminApi } from "@/lib/api"
import {
  BLACKHOLE_ADDRESS,
  MIGRATION_GUIDES,
  SDK_DOCS_URL,
  SNIPPET_LANGS,
  deriveSmtpHost,
  sendSnippet,
  smtpUsername,
  type SnippetContext,
} from "@/lib/api-p3"

export function SetupTab({ org, server }: { org: string; server: string }) {
  const credentials = useQuery({
    queryKey: ["credentials", org, server],
    queryFn: () => adminApi.credentials(org, server).list(),
  })
  const domains = useQuery({
    queryKey: ["domains", org, server],
    queryFn: () => adminApi.domains(org, server).list(),
  })
  const [lang, setLang] = useState("curl")

  const origin = typeof window !== "undefined" ? window.location.origin : ""

  const apiKey = useMemo(
    () =>
      credentials.data?.credentials.find((c) => c.type === "API" && !c.hold)?.key ??
      "YOUR_API_KEY",
    [credentials.data],
  )
  const from = useMemo(() => {
    const verified = domains.data?.domains.find((d) => d.verified) ?? domains.data?.domains[0]
    return verified ? `hello@${verified.name}` : "hello@yourdomain.com"
  }, [domains.data])
  const smtpHost = useMemo(
    () => deriveSmtpHost(domains.data?.domains, origin ? new URL(origin).hostname : "smtp.camelmailer.com"),
    [domains.data, origin],
  )

  const ctx: SnippetContext = {
    origin,
    apiKey,
    from,
    to: BLACKHOLE_ADDRESS,
    smtpHost,
    smtpUser: smtpUsername(org, server),
  }
  const snippet = sendSnippet(lang, ctx)
  const hasKey = apiKey !== "YOUR_API_KEY"

  if (credentials.isLoading) {
    return <p className="text-sm text-muted-foreground">Loading…</p>
  }

  return (
    <div className="space-y-5">
      <PageHeader
        title="Setup instructions"
        description="Send your first message in under a minute. This server's endpoint and API key are already filled in."
      />

      {!hasKey && (
        <EmptyState
          icon={KeyRoundIcon}
          title="No API credential yet"
          description="The snippets below use a placeholder. Create an API credential and it drops straight into the code."
          action={{
            label: "Create API credential",
            href: `/orgs/${org}/servers/${server}/credentials`,
          }}
        />
      )}

      {/* language picker */}
      <div className="flex flex-wrap gap-1.5">
        {SNIPPET_LANGS.map((l) => (
          <Button
            key={l.id}
            variant={lang === l.id ? "default" : "outline"}
            size="sm"
            onClick={() => setLang(l.id)}
          >
            {l.label}
          </Button>
        ))}
      </div>

      <div className="relative">
        <div className="absolute right-2 top-2 z-10">
          <CopyButton value={snippet} />
        </div>
        <pre className="overflow-auto rounded-md border bg-muted p-4 text-xs leading-relaxed">
          {snippet}
        </pre>
      </div>

      <p className="text-sm text-muted-foreground">
        Sending to{" "}
        <code className="rounded bg-muted px-1.5 py-0.5 text-xs">{BLACKHOLE_ADDRESS}</code> is
        risk-free. The Blackhole address accepts and silently discards every message, so you can
        test the full pipeline without emailing a real person.
      </p>

      <div className="grid gap-3 sm:grid-cols-2">
        <Card>
          <CardContent className="space-y-1.5 p-4">
            <h3 className="text-sm font-medium">Official SDKs</h3>
            <p className="text-sm text-muted-foreground">
              Prefer a typed client? We ship SDKs for Node, Python, PHP, Ruby, Go and more.
            </p>
            <Button variant="link" size="sm" className="h-auto p-0" asChild>
              <a href={SDK_DOCS_URL} target="_blank" rel="noreferrer">
                Browse the SDKs <ExternalLinkIcon className="size-3.5" />
              </a>
            </Button>
          </CardContent>
        </Card>
        <Card>
          <CardContent className="space-y-1.5 p-4">
            <h3 className="text-sm font-medium">Switching providers?</h3>
            <p className="text-sm text-muted-foreground">
              Drop-in migration guides for the common stacks:
            </p>
            <div className="flex flex-wrap gap-x-3 gap-y-1">
              {MIGRATION_GUIDES.map((g) => (
                <Button key={g.from} variant="link" size="sm" className="h-auto p-0" asChild>
                  <a href={g.href} target="_blank" rel="noreferrer">
                    From {g.from} <ExternalLinkIcon className="size-3.5" />
                  </a>
                </Button>
              ))}
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  )
}
