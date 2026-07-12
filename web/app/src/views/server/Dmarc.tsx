"use client"

// The DMARC tab of a mail server: pick a sending domain, see the live
// DNS health traffic lights (admin health endpoint), the compliance
// sentence + top sources (per-server DMARC endpoints), and how to set
// up the RUA ingestion route. Positioning: "EasyDMARC built in".

import { useMemo, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { InboxIcon, RefreshCwIcon } from "lucide-react"
import { CopyButton, EmptyState, formatDate, PageHeader } from "@/components/shared"
import { StatusPill } from "@/components/status-pill"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { cn } from "@/lib/utils"
import { adminApi, serverApi, type DomainHealthCheck } from "@/lib/api"

type Scope = { org: string; server: string }

/// A value with a thin underline bar underneath — the ranking style of
/// the sources table (aligned share per source IP).
function UnderlineBar({ pct }: { pct: number }) {
  const clamped = Math.min(100, Math.max(0, pct))
  return (
    <div className="ml-auto w-24 space-y-1">
      <div className="text-right text-sm tabular-nums">{pct}%</div>
      <div className="h-0.5 w-full rounded-full bg-muted">
        <div
          className={cn(
            "h-full rounded-full",
            clamped >= 90 ? "bg-emerald-500" : clamped >= 50 ? "bg-amber-500" : "bg-red-500",
          )}
          style={{ width: `${clamped}%` }}
        />
      </div>
    </div>
  )
}

function HealthCheckCard({
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
          {title} <StatusPill status={check.status} />
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
            {check.found.map((record) => (
              <code key={record} className="break-all rounded bg-muted px-2 py-1 text-xs">
                {record}
              </code>
            ))}
          </div>
        ) : (
          check.expected && (
            <div className="grid gap-1">
              <span className="text-xs text-muted-foreground">Publish:</span>
              <code className="break-all rounded bg-muted px-2 py-1 text-xs">
                {check.expected}
              </code>
            </div>
          )
        )}
        {extra}
      </CardContent>
    </Card>
  )
}

export function Dmarc({ org, server }: Scope) {
  const domains = useQuery({
    queryKey: ["domains", org, server],
    queryFn: () => adminApi.domains(org, server).list(),
  })
  const credentials = useQuery({
    queryKey: ["credentials", org, server],
    queryFn: () => adminApi.credentials(org, server).list(),
  })
  const [selected, setSelected] = useState<string | null>(null)
  const domain = selected ?? domains.data?.domains[0]?.name ?? null

  const health = useQuery({
    queryKey: ["dmarc-health", org, server, domain],
    queryFn: () => adminApi.domains(org, server).health(domain!),
    enabled: domain !== null,
  })

  // The compliance endpoints live on the per-server API; use the first
  // usable API credential, like the Messaging tab does.
  const apiKey = useMemo(
    () =>
      credentials.data?.credentials.find(
        (credential) => credential.type === "API" && !credential.hold,
      )?.key ?? null,
    [credentials.data],
  )
  const sapi = useMemo(() => (apiKey ? serverApi(apiKey) : null), [apiKey])
  const summary = useQuery({
    queryKey: ["dmarc-summary", org, server, domain, apiKey !== null],
    queryFn: () => sapi!.dmarc.summary(`?domain=${encodeURIComponent(domain!)}`),
    enabled: sapi !== null && domain !== null,
  })
  const reports = useQuery({
    queryKey: ["dmarc-reports", org, server, domain, apiKey !== null],
    queryFn: () =>
      sapi!.dmarc.reports(`?domain=${encodeURIComponent(domain!)}&per_page=10`),
    enabled: sapi !== null && domain !== null,
  })

  if (domains.data && domains.data.domains.length === 0) {
    return (
      <EmptyState>
        DMARC monitoring works per sending domain. Add one in the Domains tab first.
      </EmptyState>
    )
  }

  const result = health.data?.health
  const ruaAddress = result?.rua_address ?? "dmarc@<your-domain>"

  return (
    <div className="space-y-6">
      <PageHeader
        title="DMARC"
        description="Authentication health and aggregate-report compliance per sending domain."
        action={
          <div className="flex items-center gap-2">
            <Select value={domain ?? undefined} onValueChange={setSelected}>
              <SelectTrigger className="w-56">
                <SelectValue placeholder="Pick a domain" />
              </SelectTrigger>
              <SelectContent>
                {domains.data?.domains.map((d) => (
                  <SelectItem key={d.id} value={d.name}>
                    {d.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <Button
              variant="outline"
              size="sm"
              onClick={() => health.refetch()}
              disabled={health.isFetching || domain === null}
            >
              <RefreshCwIcon className="size-4" /> Re-check
            </Button>
          </div>
        }
      />

      {health.isLoading && <p className="text-sm text-muted-foreground">Checking DNS…</p>}

      {result && (
        <>
          <Card>
            <CardContent className="flex flex-wrap items-center gap-3 pt-6">
              <StatusPill status={result.overall} />
              <span className="text-sm">{result.next_step}</span>
            </CardContent>
          </Card>
          <div className="grid gap-4 lg:grid-cols-3">
            <HealthCheckCard title="SPF" check={result.checks.spf} />
            <HealthCheckCard title="DKIM" check={result.checks.dkim} />
            <HealthCheckCard
              title="DMARC"
              check={result.checks.dmarc}
              extra={
                result.checks.dmarc.policy && (
                  <div className="flex flex-wrap gap-1 text-xs">
                    <Badge variant="outline">p={result.checks.dmarc.policy.p ?? "?"}</Badge>
                    {result.checks.dmarc.policy.sp && (
                      <Badge variant="outline">sp={result.checks.dmarc.policy.sp}</Badge>
                    )}
                    <Badge variant="outline">pct={result.checks.dmarc.policy.pct}</Badge>
                    {result.checks.dmarc.policy.rua.map((rua) => (
                      <Badge key={rua} variant="outline">
                        {rua}
                      </Badge>
                    ))}
                  </div>
                )
              }
            />
          </div>
        </>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Compliance</CardTitle>
          <CardDescription>
            Aggregate-report data received for {domain ?? "this domain"}.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          {!sapi ? (
            <p className="text-sm text-muted-foreground">
              Compliance data uses the server&apos;s API. Create an API credential in the
              Credentials tab first.
            </p>
          ) : summary.data && summary.data.summary.total > 0 ? (
            <>
              {/* honest numbers with a denominator, as one sentence */}
              <p className="text-sm">
                Reporters covered{" "}
                <span className="font-semibold tabular-nums">
                  {summary.data.summary.total.toLocaleString()}
                </span>{" "}
                messages from {domain}. Of those,{" "}
                <span
                  className={cn(
                    "font-semibold tabular-nums",
                    summary.data.summary.pass_rate >= 0.95
                      ? "text-emerald-600 dark:text-emerald-400"
                      : "text-amber-600 dark:text-amber-400",
                  )}
                >
                  {Math.round(summary.data.summary.pass_rate * 1000) / 10}%
                </span>{" "}
                passed DMARC (DKIM or SPF aligned)
                {summary.data.summary.fail > 0 ? (
                  <>
                    ,{" "}
                    <span className="font-semibold tabular-nums text-red-600 dark:text-red-400">
                      {summary.data.summary.fail.toLocaleString()}
                    </span>{" "}
                    failed.
                  </>
                ) : (
                  <>, with no failures. Looking good.</>
                )}
              </p>
              <div className="flex flex-wrap gap-1.5">
                {Object.entries(summary.data.summary.by_disposition).map(
                  ([disposition, count]) => (
                    <Badge key={disposition} variant="outline">
                      {disposition}: {count}
                    </Badge>
                  ),
                )}
              </div>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Source IP</TableHead>
                    <TableHead className="text-right">Messages</TableHead>
                    <TableHead className="text-right">SPF aligned</TableHead>
                    <TableHead className="text-right">DKIM aligned</TableHead>
                    <TableHead>Dispositions</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {summary.data.summary.by_source.map((source) => (
                    <TableRow key={source.source_ip}>
                      <TableCell className="font-mono text-xs">{source.source_ip}</TableCell>
                      <TableCell className="text-right tabular-nums">{source.count}</TableCell>
                      <TableCell>
                        <UnderlineBar pct={source.spf_aligned_pct} />
                      </TableCell>
                      <TableCell>
                        <UnderlineBar pct={source.dkim_aligned_pct} />
                      </TableCell>
                      <TableCell className="text-muted-foreground">
                        {Object.entries(source.disposition_counts)
                          .map(([disposition, count]) => `${disposition}: ${count}`)
                          .join(", ")}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
              {reports.data && reports.data.reports.length > 0 && (
                <div>
                  <div className="mb-1 text-sm font-medium">Latest reports</div>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>Reporter</TableHead>
                        <TableHead>Period</TableHead>
                        <TableHead className="text-right">Rows</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {reports.data.reports.map((report) => (
                        <TableRow key={report.id}>
                          <TableCell>{report.org_name ?? report.org_email ?? "unknown"}</TableCell>
                          <TableCell className="text-muted-foreground">
                            {formatDate(report.date_range_begin)} –{" "}
                            {formatDate(report.date_range_end)}
                          </TableCell>
                          <TableCell className="text-right">{report.record_count}</TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              )}
            </>
          ) : (
            <p className="text-sm text-muted-foreground">
              No aggregate reports yet. They usually arrive once a day after the DMARC
              record points at your RUA address.
            </p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <InboxIcon className="size-4" /> Receive aggregate reports here
          </CardTitle>
          <CardDescription>
            Two records and the reports flow straight into this dashboard, so you do not
            need an external DMARC service.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          <div className="space-y-1">
            <p className="text-muted-foreground">
              1. Create an inbound route (Routes tab) for e.g.{" "}
              <code>dmarc@{domain ?? "your-domain"}</code> with this endpoint, so arriving
              reports are parsed and stored instead of forwarded:
            </p>
            <div className="flex items-center gap-2">
              <code className="rounded bg-muted px-2 py-1 text-xs">
                internal://dmarc-reports
              </code>
              <CopyButton value="internal://dmarc-reports" />
            </div>
          </div>
          <div className="space-y-1">
            <p className="text-muted-foreground">
              2. Point your DMARC record at that address:
            </p>
            <div className="flex items-center gap-2">
              <code className="min-w-0 break-all rounded bg-muted px-2 py-1 text-xs">
                v=DMARC1; p=none; rua=mailto:{ruaAddress}
              </code>
              <CopyButton value={`v=DMARC1; p=none; rua=mailto:${ruaAddress}`} />
            </div>
          </div>
          <p className="text-muted-foreground">
            3. Watch the compliance data above and follow the recommended next step to
            tighten the policy (none → quarantine → reject). Details in docs/dmarc.md.
          </p>
        </CardContent>
      </Card>
    </div>
  )
}
