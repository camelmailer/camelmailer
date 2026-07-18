"use client"

// The DMARC tab of a mail server: pick a sending domain, see the live
// DNS health traffic lights (admin health endpoint), the compliance
// sentence + top sources (per-server DMARC endpoints), and how to set
// up the RUA ingestion route. Positioning: "EasyDMARC built in".

import { useMemo, useState, type ReactNode } from "react"
import { useMutation, useQueries, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { CheckCircle2Icon, InboxIcon, RefreshCwIcon } from "lucide-react"
import { toast } from "sonner"
import { Area, AreaChart, CartesianGrid, XAxis } from "recharts"
import { CopyButton, EmptyState, formatDate, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
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
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  type ChartConfig,
} from "@/components/ui/chart"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { DataTable } from "@/components/ui/data-table"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import {
  adminApi,
  ApiError,
  serverApi,
  type DmarcReport,
  type DomainHealthCheck,
} from "@/lib/api"

type Scope = { org: string; server: string }

type DmarcCategory = "compliant" | "forwarded" | "noncompliant" | "threat"

const DMARC_CATEGORIES: { key: DmarcCategory; label: string; color: string }[] = [
  { key: "compliant", label: "Compliant", color: "#16a34a" },
  { key: "forwarded", label: "Forwarded", color: "#0ea5e9" },
  { key: "noncompliant", label: "Non-compliant", color: "#f59e0b" },
  { key: "threat", label: "Threat / Unknown", color: "#ef4444" },
]
const CATEGORY_LABEL: Record<DmarcCategory, string> = Object.fromEntries(
  DMARC_CATEGORIES.map((c) => [c.key, c.label]),
) as Record<DmarcCategory, string>
const CATEGORY_COLOR: Record<DmarcCategory, string> = Object.fromEntries(
  DMARC_CATEGORIES.map((c) => [c.key, c.color]),
) as Record<DmarcCategory, string>

/// DMARC outcome of a report record from its DKIM/SPF alignment (mutually
/// exclusive buckets, EasyDMARC-style).
function classify(dkimAligned: boolean, spfAligned: boolean): DmarcCategory {
  if (dkimAligned && spfAligned) return "compliant"
  if (dkimAligned) return "forwarded"
  if (spfAligned) return "noncompliant"
  return "threat"
}

/// Best-effort sending-provider name from a source IP. Aggregate DMARC data
/// carries no PTR, so this is a known-range heuristic.
function providerFor(ip: string): string {
  if (/^40\.(9[2-9]|1[0-2][0-9])\./.test(ip) || /^52\.1[0-2][0-9]\./.test(ip))
    return "Microsoft 365 / Outlook"
  if (/^(209\.85|64\.233|66\.102|66\.249|72\.14|74\.125|173\.194|108\.177)\./.test(ip))
    return "Google Workspace"
  if (/^(54\.240|76\.223|23\.249\.20|3\.5\.8)\./.test(ip)) return "Amazon SES"
  if (/^(168\.245|149\.72|167\.89|198\.37\.1)\./.test(ip)) return "SendGrid"
  if (/^185\.70\.40\./.test(ip)) return "MailerLite"
  if (/^(103\.21\.244|198\.51\.100)\./.test(ip)) return "Forwarder"
  return "Unknown source"
}

type SourceRow = {
  source_ip: string
  provider: string
  volume: number
  spfPct: number
  dkimPct: number
  category: DmarcCategory
  dispositions: Record<string, number>
}

/// A left-aligned pass-rate bar (share aligned per source).
function PctBar({ pct }: { pct: number }) {
  const clamped = Math.min(100, Math.max(0, pct))
  return (
    <div className="w-28 space-y-1">
      <div className="text-sm tabular-nums">{pct}%</div>
      <div className="h-1 w-full rounded-full bg-muted">
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

const pct = (x: number) => `${Math.round(x * 1000) / 10}%`

const dmarcChartConfig: ChartConfig = Object.fromEntries(
  DMARC_CATEGORIES.map((c) => [c.key, { label: c.label, color: c.color }]),
)

/// A headline metric card (compliance / volume / pass rates).
function StatCard({
  label,
  value,
  sub,
  tone,
}: {
  label: string
  value: string
  sub?: string
  tone?: "good" | "warn"
}) {
  return (
    <Card>
      <CardContent className="p-4">
        <p className="text-xs font-medium text-muted-foreground">{label}</p>
        <p
          className={cn(
            "mt-1 text-2xl font-semibold tabular-nums",
            tone === "good" && "text-emerald-600 dark:text-emerald-400",
            tone === "warn" && "text-amber-600 dark:text-amber-400",
          )}
        >
          {value}
        </p>
        {sub && <p className="mt-0.5 text-xs text-muted-foreground">{sub}</p>}
      </CardContent>
    </Card>
  )
}

/// A selectable category filter chip with a volume count.
function CategoryChip({
  label,
  count,
  active,
  onClick,
  color,
}: {
  label: string
  count: number
  active: boolean
  onClick: () => void
  color?: string
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        "flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs transition-colors",
        active
          ? "border-primary bg-primary/10 text-foreground"
          : "text-muted-foreground hover:text-foreground",
      )}
    >
      {color && <span className="size-2 rounded-full" style={{ background: color }} />}
      {label}
      <span className="tabular-nums">{count.toLocaleString()}</span>
    </button>
  )
}

function middleTruncate(value: string, max = 48): string {
  if (value.length <= max) return value
  const half = Math.floor((max - 1) / 2)
  return `${value.slice(0, half)}…${value.slice(value.length - half)}`
}

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

/// SPF / DKIM / DMARC card in the same shape as the domain detail: the
/// record to publish (or the one found live) with its status + problems.
function HealthCheckCard({
  title,
  purpose,
  check,
  extra,
}: {
  title: string
  purpose: string
  check: DomainHealthCheck
  extra?: ReactNode
}) {
  const recordValue = check.found[0] ?? check.expected ?? ""
  return (
    <Card>
      <CardHeader className="pb-3">
        <div className="flex items-center justify-between gap-2">
          <CardTitle className="text-base">{title}</CardTitle>
          <StatusPill status={check.status} />
        </div>
        <CardDescription>{purpose}</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-3">
        {recordValue && (
          <div className="grid gap-1.5 rounded-md border p-3">
            <Badge variant="outline" className="w-fit">
              TXT
            </Badge>
            <MonoValue label="Name" value={check.record_name} />
            <MonoValue label="Value" value={recordValue} />
          </div>
        )}
        {check.problems.length > 0 && (
          <ul className="list-disc space-y-1 pl-5 text-xs text-muted-foreground">
            {check.problems.map((problem) => (
              <li key={problem}>{problem}</li>
            ))}
          </ul>
        )}
        {extra}
      </CardContent>
    </Card>
  )
}

const sourceColumns: ColumnDef<SourceRow>[] = [
  {
    id: "provider",
    header: "Sending source",
    accessorFn: (s) => s.provider,
    cell: ({ row }) => (
      <div className="min-w-0">
        <div className="flex items-center gap-2">
          <span className="truncate font-medium">{row.original.provider}</span>
          <StatusPill
            status={CATEGORY_LABEL[row.original.category]}
            tone={
              row.original.category === "compliant"
                ? "green"
                : row.original.category === "threat"
                  ? "red"
                  : row.original.category === "noncompliant"
                    ? "amber"
                    : "teal"
            }
          />
        </div>
        <span className="block truncate font-mono text-xs text-muted-foreground">
          {row.original.source_ip}
        </span>
      </div>
    ),
  },
  {
    id: "volume",
    header: "Volume",
    accessorFn: (s) => s.volume,
    meta: { align: "right" },
    cell: ({ row }) => <span className="tabular-nums">{row.original.volume.toLocaleString()}</span>,
  },
  {
    id: "spf",
    header: "SPF pass",
    accessorFn: (s) => s.spfPct,
    cell: ({ row }) => <PctBar pct={row.original.spfPct} />,
  },
  {
    id: "dkim",
    header: "DKIM pass",
    accessorFn: (s) => s.dkimPct,
    cell: ({ row }) => <PctBar pct={row.original.dkimPct} />,
  },
  {
    id: "dispositions",
    header: "Dispositions",
    enableSorting: false,
    accessorFn: (s) =>
      Object.entries(s.dispositions)
        .map(([disposition, count]) => `${disposition}: ${count}`)
        .join(", "),
    cell: ({ row }) => (
      <span className="text-xs text-muted-foreground">
        {Object.entries(row.original.dispositions)
          .map(([disposition, count]) => `${disposition}: ${count}`)
          .join(", ")}
      </span>
    ),
  },
]

const reportColumns: ColumnDef<DmarcReport>[] = [
  {
    id: "reporter",
    header: "Reporter",
    accessorFn: (r) => r.org_name ?? r.org_email ?? "unknown",
    cell: ({ row }) => (
      <span className="block max-w-64 truncate font-medium transition-colors group-hover:text-primary">
        {row.original.org_name ?? row.original.org_email ?? "unknown"}
      </span>
    ),
  },
  {
    id: "period",
    header: "Period",
    accessorFn: (r) => r.date_range_begin,
    cell: ({ row }) => (
      <span className="whitespace-nowrap text-muted-foreground">
        {formatDate(row.original.date_range_begin)} – {formatDate(row.original.date_range_end)}
      </span>
    ),
  },
  {
    id: "rows",
    header: "Rows",
    accessorFn: (r) => r.record_count,
    meta: { align: "right" },
    cell: ({ row }) => <span>{row.original.record_count}</span>,
  },
]

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
  const reports = useQuery({
    queryKey: ["dmarc-reports", org, server, domain, apiKey !== null],
    queryFn: () =>
      sapi!.dmarc.reports(`?domain=${encodeURIComponent(domain!)}&per_page=50`),
    enabled: sapi !== null && domain !== null,
  })

  // Per-report records → accurate categories, provider grouping and a daily
  // compliance time series (the aggregate summary has no per-day breakdown).
  const reportDetails = useQueries({
    queries: (reports.data?.reports ?? []).map((r) => ({
      queryKey: ["dmarc-report", org, server, r.id, apiKey !== null],
      queryFn: () => sapi!.dmarc.report(r.id),
      enabled: sapi !== null,
    })),
  })
  const detailsLoading =
    reports.isPending || (reports.data?.reports.length ? reportDetails.some((q) => q.isPending) : false)
  // A stable signal that recomputes the aggregate only when data actually changed.
  const detailsSignature = reportDetails.map((q) => q.dataUpdatedAt).join(",")
  const agg = useMemo(() => {
    let volume = 0
    let spfA = 0
    let dkimA = 0
    const cat: Record<DmarcCategory, number> = { compliant: 0, forwarded: 0, noncompliant: 0, threat: 0 }
    const bySrc = new Map<
      string,
      { volume: number; spfA: number; dkimA: number; disp: Record<string, number>; cat: Record<DmarcCategory, number> }
    >()
    const byDay = new Map<string, Record<DmarcCategory, number>>()
    for (const q of reportDetails) {
      const d = q.data
      if (!d) continue
      const day = d.report.date_range_begin.slice(0, 10)
      const dayB = byDay.get(day) ?? { compliant: 0, forwarded: 0, noncompliant: 0, threat: 0 }
      for (const rec of d.records) {
        const c = classify(rec.dkim_aligned, rec.spf_aligned)
        const n = rec.count
        volume += n
        cat[c] += n
        dayB[c] += n
        if (rec.spf_aligned) spfA += n
        if (rec.dkim_aligned) dkimA += n
        const s =
          bySrc.get(rec.source_ip) ??
          { volume: 0, spfA: 0, dkimA: 0, disp: {}, cat: { compliant: 0, forwarded: 0, noncompliant: 0, threat: 0 } }
        s.volume += n
        if (rec.spf_aligned) s.spfA += n
        if (rec.dkim_aligned) s.dkimA += n
        s.disp[rec.disposition] = (s.disp[rec.disposition] ?? 0) + n
        s.cat[c] += n
        bySrc.set(rec.source_ip, s)
      }
      byDay.set(day, dayB)
    }
    const sources: SourceRow[] = [...bySrc.entries()]
      .map(([ip, s]) => ({
        source_ip: ip,
        provider: providerFor(ip),
        volume: s.volume,
        spfPct: s.volume ? Math.round((s.spfA / s.volume) * 100) : 0,
        dkimPct: s.volume ? Math.round((s.dkimA / s.volume) * 100) : 0,
        category: (Object.entries(s.cat).sort((a, b) => b[1] - a[1])[0]?.[0] ?? "threat") as DmarcCategory,
        dispositions: s.disp,
      }))
      .sort((a, b) => b.volume - a.volume)
    const series = [...byDay.entries()]
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([date, v]) => ({ date, ...v }))
    return { volume, spfA, dkimA, cat, sources, series }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [detailsSignature])

  const complianceRate = agg.volume ? (agg.volume - agg.cat.threat) / agg.volume : 0
  const spfRate = agg.volume ? agg.spfA / agg.volume : 0
  const dkimRate = agg.volume ? agg.dkimA / agg.volume : 0
  const [category, setCategory] = useState<DmarcCategory | "all">("all")
  const filteredSources =
    category === "all" ? agg.sources : agg.sources.filter((s) => s.category === category)

  // One-click ingestion: the inbound route that parses aggregate reports.
  const queryClient = useQueryClient()
  const routes = useQuery({
    queryKey: ["routes", org, server],
    queryFn: () => adminApi.routes(org, server).list(),
  })
  const domainId = domains.data?.domains.find((d) => d.name === domain)?.id ?? null
  const dmarcRoute = routes.data?.routes.find(
    (r) =>
      r.endpoint_url === "internal://dmarc-reports" &&
      (domainId ? r.domain_id === domainId : true),
  )
  const createDmarcRoute = useMutation({
    mutationFn: () =>
      adminApi.routes(org, server).create({
        name: "dmarc",
        mode: "Endpoint",
        ...(domain ? { domain } : {}),
        endpoint_url: "internal://dmarc-reports",
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["routes", org, server] })
      toast.success("Inbound route created — DMARC reports will be parsed here")
    },
    onError: (err) =>
      toast.error(err instanceof ApiError ? err.message : "Could not create the route"),
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
    <Page
      header={
        <PageHeader
          title="DMARC"
          description="Authentication health and aggregate-report compliance per sending domain."
          className="mb-0"
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
                onClick={async () => {
                  await health.refetch()
                  toast.success("DNS re-checked")
                }}
                disabled={health.isFetching || domain === null}
              >
                <RefreshCwIcon className={cn("size-4", health.isFetching && "animate-spin")} />
                {health.isFetching ? "Checking…" : "Re-check"}
              </Button>
            </div>
          }
        />
      }
    >
      <div className="space-y-6">
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
            <HealthCheckCard
              title="SPF"
              purpose="Authorizes this server to send mail for the domain."
              check={result.checks.spf}
            />
            <HealthCheckCard
              title="DKIM"
              purpose="Cryptographically signs your outgoing mail."
              check={result.checks.dkim}
            />
            <HealthCheckCard
              title="DMARC"
              purpose="Tells receivers what to do with mail that fails SPF or DKIM."
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

      {!sapi ? (
        <Card>
          <CardContent className="pt-6">
            <p className="text-sm text-muted-foreground">
              Compliance data uses the server&apos;s API. Create an API credential in the
              Credentials tab first.
            </p>
          </CardContent>
        </Card>
      ) : detailsLoading ? (
        <div className="grid gap-4">
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-24 rounded-lg" />
            ))}
          </div>
          <Skeleton className="h-72 rounded-lg" />
          <Skeleton className="h-72 rounded-lg" />
        </div>
      ) : agg.volume === 0 ? (
        <Card>
          <CardContent className="pt-6">
            <p className="text-sm text-muted-foreground">
              No aggregate reports yet. They usually arrive once a day after the DMARC record
              points at your RUA address.
            </p>
          </CardContent>
        </Card>
      ) : (
        <>
          {/* headline metrics */}
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
            <StatCard
              label="DMARC compliance"
              value={pct(complianceRate)}
              tone={complianceRate >= 0.95 ? "good" : "warn"}
              sub={`${agg.volume.toLocaleString()} messages`}
            />
            <StatCard
              label="Volume"
              value={agg.volume.toLocaleString()}
              sub={`${agg.series.length} days`}
            />
            <StatCard
              label="SPF pass rate"
              value={pct(spfRate)}
              tone={spfRate >= 0.9 ? "good" : "warn"}
            />
            <StatCard
              label="DKIM pass rate"
              value={pct(dkimRate)}
              tone={dkimRate >= 0.9 ? "good" : "warn"}
            />
          </div>

          {/* volume over time by compliance */}
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-base">Email volume by DMARC compliance</CardTitle>
              <CardDescription>
                Daily volume for {domain}, split by authentication outcome.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <ChartContainer config={dmarcChartConfig} className="h-64 w-full">
                <AreaChart data={agg.series} margin={{ left: 4, right: 8, top: 8 }}>
                  <CartesianGrid vertical={false} strokeDasharray="3 3" />
                  <XAxis
                    dataKey="date"
                    tickLine={false}
                    axisLine={false}
                    tickMargin={8}
                    tickFormatter={(d) =>
                      new Date(d).toLocaleDateString(undefined, { month: "short", day: "numeric" })
                    }
                  />
                  <ChartTooltip content={<ChartTooltipContent />} />
                  {DMARC_CATEGORIES.map((c) => (
                    <Area
                      key={c.key}
                      dataKey={c.key}
                      stackId="v"
                      type="monotone"
                      stroke={`var(--color-${c.key})`}
                      fill={`var(--color-${c.key})`}
                      fillOpacity={0.25}
                    />
                  ))}
                </AreaChart>
              </ChartContainer>
              <div className="mt-3 flex flex-wrap gap-x-4 gap-y-1.5 text-xs">
                {DMARC_CATEGORIES.map((c) => (
                  <span key={c.key} className="flex items-center gap-1.5">
                    <span className="size-2.5 rounded-full" style={{ background: c.color }} />
                    {c.label}{" "}
                    <span className="tabular-nums text-muted-foreground">
                      {agg.cat[c.key].toLocaleString()}
                    </span>
                  </span>
                ))}
              </div>
            </CardContent>
          </Card>

          {/* sending sources */}
          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-base">Sending sources</CardTitle>
              <CardDescription>Who sent as {domain}, and how they authenticated.</CardDescription>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="flex flex-wrap gap-1.5">
                <CategoryChip
                  label="All"
                  count={agg.volume}
                  active={category === "all"}
                  onClick={() => setCategory("all")}
                />
                {DMARC_CATEGORIES.map((c) => (
                  <CategoryChip
                    key={c.key}
                    label={c.label}
                    count={agg.cat[c.key]}
                    color={c.color}
                    active={category === c.key}
                    onClick={() => setCategory(c.key)}
                  />
                ))}
              </div>
              <DataTable
                columns={sourceColumns}
                data={filteredSources}
                searchKeys={["source_ip", "provider"]}
                searchPlaceholder="Search sources…"
                emptyText="No sources in this category."
                initialPageSize={20}
              />
            </CardContent>
          </Card>

          {/* aggregate reports received */}
          {reports.data && reports.data.reports.length > 0 && (
            <Card>
              <CardHeader className="pb-3">
                <CardTitle className="text-base">Aggregate reports</CardTitle>
                <CardDescription>Reporters that sent DMARC data for {domain}.</CardDescription>
              </CardHeader>
              <CardContent>
                <DataTable
                  columns={reportColumns}
                  data={reports.data.reports}
                  loading={reports.isPending}
                  searchKeys={["org_name", "org_email"]}
                  searchPlaceholder="Search reporters…"
                  emptyText="No reports match your search."
                  initialPageSize={10}
                />
              </CardContent>
            </Card>
          )}
        </>
      )}

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
          <div className="space-y-2">
            <p className="text-muted-foreground">
              1. Receive reports at <code>dmarc@{domain ?? "your-domain"}</code>. One click sets
              up an inbound route to <code>internal://dmarc-reports</code>, so arriving reports
              are parsed and stored here instead of forwarded.
            </p>
            {dmarcRoute ? (
              <div className="flex items-center gap-2 text-sm font-medium text-emerald-600 dark:text-emerald-400">
                <CheckCircle2Icon className="size-4" />
                Inbound route ready — dmarc@{domain} → internal://dmarc-reports
              </div>
            ) : (
              <Button
                size="sm"
                onClick={() => createDmarcRoute.mutate()}
                disabled={createDmarcRoute.isPending || !domainId}
              >
                <InboxIcon className="size-4" />
                {createDmarcRoute.isPending ? "Setting up…" : "Set up inbound route"}
              </Button>
            )}
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
    </Page>
  )
}
