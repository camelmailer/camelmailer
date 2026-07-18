"use client"

// Domain detail — the critical DNS flow: records grouped by purpose
// (verification / sending) with per-record status pills, monospace
// values with middle truncation + copy, a Verify action that surfaces
// the exact missing record, live health traffic lights (SPF/DKIM/DMARC)
// and the delegation flow ("email the records to whoever owns the DNS").

import { useState, type ReactNode } from "react"
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
import { Skeleton } from "@/components/ui/skeleton"
import { Textarea } from "@/components/ui/textarea"
import {
  adminApi,
  ApiError,
  type DnsRecord,
  type HealthStatus,
} from "@/lib/api"
import { dnsInstructionsMailto, getDomain } from "@/lib/api-p2"
import { cn } from "@/lib/utils"

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

/** Status pill for a live DNS check — a skeleton while the check loads. */
function CheckPill({ status, loading }: { status?: HealthStatus; loading: boolean }) {
  if (loading) return <Skeleton className="h-5 w-16 rounded-full" />
  if (!status) return <StatusPill status="unchecked" />
  return healthPill(status)
}

/** A DNS record combined with its live status: what to publish plus whether
 *  it is already live, in one card (verification / SPF / DKIM / DMARC). */
function RecordCard({
  title,
  purpose,
  pill,
  record,
  problems,
  extra,
}: {
  title: string
  purpose: string
  pill: ReactNode
  record?: DnsRecord | null
  problems?: string[]
  extra?: ReactNode
}) {
  const hasProblems = !!problems && problems.length > 0
  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between gap-2">
          <CardTitle className="text-base">{title}</CardTitle>
          {pill}
        </div>
        <CardDescription>{purpose}</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-3">
        {record ? (
          <div className="grid gap-1.5 rounded-md border p-3">
            <Badge variant="outline" className="w-fit">
              {record.type}
            </Badge>
            <MonoValue label="Name" value={record.name} />
            <MonoValue label="Value" value={record.value} />
          </div>
        ) : hasProblems ? null : (
          <div className="grid gap-2 rounded-md border p-3">
            <Skeleton className="h-5 w-12" />
            <Skeleton className="h-6 w-full" />
          </div>
        )}
        {hasProblems && (
          <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
            {problems!.map((problem) => (
              <li key={problem}>{problem}</li>
            ))}
          </ul>
        )}
        {extra}
      </CardContent>
    </Card>
  )
}

function RecordCardSkeleton() {
  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between gap-2">
          <Skeleton className="h-5 w-28" />
          <Skeleton className="h-5 w-16 rounded-full" />
        </div>
        <Skeleton className="h-4 w-3/4" />
      </CardHeader>
      <CardContent>
        <div className="grid gap-2 rounded-md border p-3">
          <Skeleton className="h-5 w-12" />
          <Skeleton className="h-6 w-full" />
          <Skeleton className="h-6 w-5/6" />
        </div>
      </CardContent>
    </Card>
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
                onClick={async () => {
                  await health.refetch()
                  toast.success("DNS re-checked")
                }}
                disabled={health.isFetching}
              >
                <RefreshCwIcon className={cn("size-4", health.isFetching && "animate-spin")} />
                {health.isFetching ? "Checking…" : "Re-check DNS"}
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

      {/* Overall DNS health summary */}
      {health.isPending ? (
        <Skeleton className="h-16 w-full rounded-lg" />
      ) : result ? (
        <Alert>
          <BadgeCheckIcon className="size-4" />
          <AlertTitle className="flex items-center gap-2">
            DNS health {healthPill(result.overall)}
          </AlertTitle>
          <AlertDescription>{result.next_step}</AlertDescription>
        </Alert>
      ) : null}

      {/* One card per record: what to publish + its live status */}
      <div className="grid gap-4 lg:grid-cols-2">
        {domainQuery.isPending ? (
          <>
            <RecordCardSkeleton />
            <RecordCardSkeleton />
            <RecordCardSkeleton />
            <RecordCardSkeleton />
          </>
        ) : domain ? (
          <>
            <RecordCard
              title="Domain verification"
              purpose="Proves you control the domain. Publish this TXT record, then hit Verify."
              pill={
                domain.verified ? (
                  <StatusPill status="verified" />
                ) : (
                  <StatusPill status="pending" />
                )
              }
              record={domain.verification_record}
            />
            <RecordCard
              title="SPF"
              purpose="Authorizes this server to send mail for the domain."
              pill={<CheckPill status={result?.checks.spf.status} loading={health.isPending} />}
              record={domain.spf_record}
              problems={result?.checks.spf.problems}
            />
            <RecordCard
              title="DKIM"
              purpose="Cryptographically signs your outgoing mail."
              pill={
                domain.dkim_record ? (
                  <CheckPill status={result?.checks.dkim.status} loading={health.isPending} />
                ) : (
                  <StatusPill status="missing" />
                )
              }
              record={domain.dkim_record}
              problems={
                domain.dkim_record
                  ? result?.checks.dkim.problems
                  : [
                      "Neither this domain nor the installation has a DKIM signing key, so outgoing mail will not be DKIM-signed.",
                    ]
              }
            />
            <RecordCard
              title="DMARC"
              purpose="Tells receivers what to do with mail that fails SPF or DKIM."
              pill={<CheckPill status={result?.checks.dmarc.status} loading={health.isPending} />}
              record={
                result
                  ? {
                      type: "TXT",
                      name: result.checks.dmarc.record_name,
                      value:
                        result.checks.dmarc.found[0] ?? result.checks.dmarc.expected ?? "",
                    }
                  : null
              }
              problems={result?.checks.dmarc.problems}
              extra={
                result?.checks.dmarc.policy && (
                  <div className="flex flex-wrap gap-1 text-xs">
                    <Badge variant="outline">p={result.checks.dmarc.policy.p ?? "?"}</Badge>
                    {result.checks.dmarc.policy.sp && (
                      <Badge variant="outline">sp={result.checks.dmarc.policy.sp}</Badge>
                    )}
                    <Badge variant="outline">pct={result.checks.dmarc.policy.pct}</Badge>
                  </div>
                )
              }
            />
          </>
        ) : null}
      </div>

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
