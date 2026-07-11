"use client"

// The DMARC tab of a mail server: pick a sending domain, see the live
// DNS health traffic lights (admin health endpoint), the compliance
// summary and top sources (per-server DMARC endpoints), and how to set
// up the RUA ingestion route.

import { useMemo, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import { RefreshCwIcon } from "lucide-react"
import { EmptyState, formatDate, PageHeader } from "@/components/shared"
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
import {
  adminApi,
  serverApi,
  type DomainHealthCheck,
  type HealthStatus,
} from "@/lib/api"

type Scope = { org: string; server: string }

function StatusBadge({ status }: { status: HealthStatus }) {
  if (status === "ok") return <Badge>ok</Badge>
  if (status === "warning") return <Badge variant="secondary">warning</Badge>
  return <Badge variant="destructive">missing</Badge>
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
          {title} <StatusBadge status={check.status} />
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
        DMARC monitoring works per sending domain — add one in the Domains tab first.
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
              <StatusBadge status={result.overall} />
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
              Compliance data uses the server&apos;s API — create an API credential in the
              Credentials tab first.
            </p>
          ) : summary.data && summary.data.summary.total > 0 ? (
            <>
              <div className="flex flex-wrap gap-6 text-sm">
                <div>
                  <div className="text-2xl font-semibold">{summary.data.summary.total}</div>
                  <div className="text-muted-foreground">messages covered</div>
                </div>
                <div>
                  <div className="text-2xl font-semibold">
                    {Math.round(summary.data.summary.pass_rate * 1000) / 10}%
                  </div>
                  <div className="text-muted-foreground">DMARC pass (DKIM + SPF aligned)</div>
                </div>
                {Object.entries(summary.data.summary.by_disposition).map(([disposition, count]) => (
                  <div key={disposition}>
                    <div className="text-2xl font-semibold">{count}</div>
                    <div className="text-muted-foreground">disposition: {disposition}</div>
                  </div>
                ))}
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
                      <TableCell className="text-right">{source.count}</TableCell>
                      <TableCell className="text-right">{source.spf_aligned_pct}%</TableCell>
                      <TableCell className="text-right">{source.dkim_aligned_pct}%</TableCell>
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
              No aggregate reports yet — reports usually arrive once a day after the DMARC
              record points at your RUA address.
            </p>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Receive aggregate reports here</CardTitle>
        </CardHeader>
        <CardContent className="space-y-2 text-sm text-muted-foreground">
          <p>
            1. Create an inbound route (Routes tab) for e.g.{" "}
            <code>dmarc@{domain ?? "your-domain"}</code> with the endpoint{" "}
            <code>internal://dmarc-reports</code> — arriving reports are parsed and stored
            instead of being forwarded.
          </p>
          <p>
            2. Put that address into your DMARC record:{" "}
            <code className="break-all rounded bg-muted px-1 py-0.5 text-xs">
              v=DMARC1; p=none; rua=mailto:{ruaAddress}
            </code>
          </p>
          <p>
            3. Watch the compliance data above and follow the recommended next step to
            tighten the policy (none → quarantine → reject). Details in docs/dmarc.md.
          </p>
        </CardContent>
      </Card>
    </div>
  )
}
