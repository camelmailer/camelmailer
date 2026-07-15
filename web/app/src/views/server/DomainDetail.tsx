"use client"

// Domain detail — the critical DNS flow: records grouped by purpose
// (verification / sending) with per-record status pills, monospace
// values with middle truncation + copy, a Verify action that surfaces
// the exact missing record, live health traffic lights (SPF/DKIM/DMARC)
// and the delegation flow ("email the records to whoever owns the DNS").

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  BadgeCheckIcon,
  MailIcon,
  RefreshCwIcon,
  SendIcon,
} from "lucide-react"
import { toast } from "sonner"
import { CopyButton, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { FormDialog } from "@/components/form-dialog"
import { StatusPill } from "@/components/status-pill"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import {
  adminApi,
  ApiError,
  type DnsRecord,
  type DomainHealth,
  type DomainHealthCheck,
  type HealthStatus,
} from "@/lib/api"
import { dnsInstructionsMailto, getDomain } from "@/lib/api-p2"

type Scope = { org: string; server: string; name: string }

// ------------------------------------------------------------- helpers

function middleTruncate(value: string, max = 60): string {
  if (value.length <= max) return value
  const half = Math.floor((max - 1) / 2)
  return `${value.slice(0, half)}…${value.slice(value.length - half)}`
}

function healthPill(status: HealthStatus) {
  if (status === "ok") return <StatusPill status="verified" />
  if (status === "warning") return <StatusPill status="warning" />
  return <StatusPill status="missing" />
}

/// One monospace name/value line with full-value copy; long values get
/// a middle […] truncation (the copied value is always complete).
function MonoValue({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex min-w-0 items-center gap-2">
      <span className="w-12 shrink-0 text-xs text-muted-foreground">{label}</span>
      <code className="min-w-0 rounded bg-muted px-2 py-1 text-xs" title={value}>
        {middleTruncate(value)}
      </code>
      <CopyButton value={value} />
    </div>
  )
}

function RecordRow({ record, pill }: { record: DnsRecord; pill: React.ReactNode }) {
  return (
    <div className="flex flex-wrap items-start justify-between gap-3 rounded-md border p-3">
      <div className="grid min-w-0 flex-1 gap-1.5">
        <div className="flex items-center gap-2">
          <Badge variant="outline">{record.type}</Badge>
          {pill}
        </div>
        <MonoValue label="Name" value={record.name} />
        <MonoValue label="Value" value={record.value} />
      </div>
    </div>
  )
}

// ------------------------------------------------------------ sections

function HealthCard({
  title,
  check,
  extra,
}: {
  title: string
  check: DomainHealthCheck
  extra?: React.ReactNode
}) {
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="flex items-center justify-between text-base">
          {title} {healthPill(check.status)}
        </CardTitle>
        <CardDescription className="break-all font-mono text-xs">
          {check.record_name}
        </CardDescription>
      </CardHeader>
      <CardContent className="grid gap-2 text-sm">
        {check.problems.length > 0 && (
          <ul className="list-disc space-y-1 pl-4 text-muted-foreground">
            {check.problems.map((problem) => (
              <li key={problem}>{problem}</li>
            ))}
          </ul>
        )}
        {check.found.length > 0 ? (
          <div className="grid gap-1">
            <span className="text-xs text-muted-foreground">Found:</span>
            {check.found.map((record) => (
              <code key={record} className="break-all rounded bg-muted px-2 py-1 text-xs">
                {record}
              </code>
            ))}
          </div>
        ) : (
          check.expected && (
            <div className="grid gap-1">
              <span className="text-xs text-muted-foreground">Expected:</span>
              <div className="flex items-start gap-2">
                <code className="min-w-0 flex-1 break-all rounded bg-muted px-2 py-1 text-xs">
                  {check.expected}
                </code>
                <CopyButton value={check.expected} />
              </div>
            </div>
          )
        )}
        {extra}
      </CardContent>
    </Card>
  )
}

function HealthTab({ health, refetching, onRecheck }: {
  health: DomainHealth | undefined
  refetching: boolean
  onRecheck: () => void
}) {
  if (!health) {
    return <p className="text-sm text-muted-foreground">Checking DNS…</p>
  }
  return (
    <div className="space-y-4">
      <Alert>
        <BadgeCheckIcon className="size-4" />
        <AlertTitle className="flex items-center gap-2">
          Overall {healthPill(health.overall)}
        </AlertTitle>
        <AlertDescription>{health.next_step}</AlertDescription>
      </Alert>
      <div className="grid gap-4 lg:grid-cols-3">
        <HealthCard title="SPF" check={health.checks.spf} />
        <HealthCard title="DKIM" check={health.checks.dkim} />
        <HealthCard
          title="DMARC"
          check={health.checks.dmarc}
          extra={
            health.checks.dmarc.policy && (
              <div className="flex flex-wrap gap-1 text-xs">
                <Badge variant="outline">p={health.checks.dmarc.policy.p ?? "?"}</Badge>
                {health.checks.dmarc.policy.sp && (
                  <Badge variant="outline">sp={health.checks.dmarc.policy.sp}</Badge>
                )}
                <Badge variant="outline">pct={health.checks.dmarc.policy.pct}</Badge>
              </div>
            )
          }
        />
      </div>
      <Button variant="outline" size="sm" onClick={onRecheck} disabled={refetching}>
        <RefreshCwIcon className="size-4" /> Re-check DNS
      </Button>
    </div>
  )
}

// ---------------------------------------------------------------- view

export function DomainDetail({ org, server, name }: Scope) {
  const queryClient = useQueryClient()
  const domainQuery = useQuery({
    queryKey: ["domain", org, server, name],
    queryFn: () => getDomain(org, server, name),
  })
  const health = useQuery({
    queryKey: ["domain-health", org, server, name],
    queryFn: () => adminApi.domains(org, server).health(name),
  })
  const [tab, setTab] = useState("records")
  const [verifyError, setVerifyError] = useState<string | null>(null)
  const [emailOpen, setEmailOpen] = useState(false)
  const [recipient, setRecipient] = useState("")
  const [note, setNote] = useState("")

  const domain = domainQuery.data?.domain
  const result = health.data?.health

  const verify = useMutation({
    mutationFn: () => adminApi.domains(org, server).verify(name),
    onSuccess: () => {
      setVerifyError(null)
      queryClient.invalidateQueries({ queryKey: ["domain", org, server, name] })
      queryClient.invalidateQueries({ queryKey: ["domains", org, server] })
      toast.success(`${name} verified`)
    },
    onError: (err) => {
      // The 422 message names the exact record to publish — show it
      // inline instead of a transient toast.
      setVerifyError(err instanceof ApiError ? err.message : "Verification failed")
    },
  })

  /// Pill of the SPF/DKIM records: graded by the live health check
  /// when available, "unchecked" until then.
  const sendingPill = (status: HealthStatus | undefined) =>
    status ? healthPill(status) : <StatusPill status="unchecked" />

  return (
    <Page
      header={
        <PageHeader
          className="mb-0 items-start"
          backHref={`/orgs/${org}/servers/${server}/domains`}
          backLabel="Domains"
          title={name}
          description={
            domain &&
            (domain.verified ? (
              <StatusPill status="verified" />
            ) : (
              <StatusPill status="unverified" />
            ))
          }
          action={
            <>
              <Button
                variant="outline"
                size="sm"
                onClick={() => health.refetch()}
                disabled={health.isFetching}
              >
                <RefreshCwIcon className="size-4" /> Re-check DNS
              </Button>
              <Button variant="outline" size="sm" onClick={() => setEmailOpen(true)}>
                <MailIcon className="size-4" /> Email instructions to a teammate
              </Button>
              {domain && !domain.verified && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => verify.mutate()}
                  disabled={verify.isPending}
                >
                  <BadgeCheckIcon className="size-4" />
                  {verify.isPending ? "Verifying…" : "Verify"}
                </Button>
              )}
            </>
          }
        />
      }
    >
      <div className="space-y-4">
      {verifyError && (
        <Alert variant="destructive">
          <AlertTitle>Not verified yet</AlertTitle>
          <AlertDescription>{verifyError}</AlertDescription>
        </Alert>
      )}

      <Tabs value={tab} onValueChange={setTab}>
        <TabsList className="mb-4">
          <TabsTrigger value="records">Records</TabsTrigger>
          <TabsTrigger value="health">Health</TabsTrigger>
        </TabsList>
      </Tabs>

      {tab === "records" &&
        (domain ? (
          <div className="space-y-6">
            <section className="space-y-2">
              <h2 className="text-sm font-medium">Domain verification</h2>
              <p className="text-sm text-muted-foreground">
                Proves you control the domain. Publish this TXT record, then hit Verify.
              </p>
              <RecordRow
                record={domain.verification_record}
                pill={
                  domain.verified ? (
                    <StatusPill status="verified" />
                  ) : (
                    <StatusPill status="pending" />
                  )
                }
              />
            </section>
            <section className="space-y-2">
              <h2 className="text-sm font-medium">Sending</h2>
              <p className="text-sm text-muted-foreground">
                SPF and DKIM authenticate the mail itself; both should be in place before
                real traffic.
              </p>
              <div className="grid gap-2">
                <RecordRow
                  record={domain.spf_record}
                  pill={sendingPill(result?.checks.spf.status)}
                />
                {domain.dkim_record ? (
                  <RecordRow
                    record={domain.dkim_record}
                    pill={sendingPill(result?.checks.dkim.status)}
                  />
                ) : (
                  <Alert>
                    <AlertTitle>No DKIM key</AlertTitle>
                    <AlertDescription>
                      Neither this domain nor the installation has a DKIM signing key, so
                      outgoing mail will not be DKIM-signed.
                    </AlertDescription>
                  </Alert>
                )}
              </div>
            </section>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">Loading…</p>
        ))}

      {tab === "health" && (
        <HealthTab
          health={result}
          refetching={health.isFetching}
          onRecheck={() => health.refetch()}
        />
      )}

      <FormDialog
        open={emailOpen}
        onOpenChange={setEmailOpen}
        title="Email the DNS records to a teammate"
        description="Opens a prefilled draft in your mail client with all records as plain text, which helps when someone else owns the DNS."
        submitLabel="Open draft"
        submitDisabled={!recipient.includes("@") || !domain}
        onSubmit={() => {
          if (!domain) return
          window.location.href = dnsInstructionsMailto(domain, recipient, note)
          setEmailOpen(false)
          toast.success("Draft opened in your mail client")
        }}
      >
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label>Recipient</Label>
            <Input
              type="email"
              value={recipient}
              onChange={(e) => setRecipient(e.target.value)}
              placeholder="dns-admin@acme.com"
            />
          </div>
          <div className="grid gap-2">
            <Label>Message (optional)</Label>
            <Textarea
              rows={3}
              value={note}
              onChange={(e) => setNote(e.target.value)}
              placeholder="Context for your teammate…"
            />
          </div>
          <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
            <SendIcon className="size-3.5" /> Sent from your own mail client, so nothing
            leaves this app.
          </p>
        </div>
      </FormDialog>
      </div>
    </Page>
  )
}
