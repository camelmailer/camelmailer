"use client"

// The org-home dashboard (masterplan §4.1): a KPI card row + a large
// stacked delivery chart, driven by REAL numbers from the first/active
// server's messaging API. No credential / no server ⇒ the onboarding
// situation shows instead — never fabricated data.

import { useMemo, useState } from "react"
import { useQuery } from "@tanstack/react-query"
import Link from "next/link"
import {
  Area,
  AreaChart,
  CartesianGrid,
  Line,
  LineChart,
  ReferenceLine,
  XAxis,
  YAxis,
} from "recharts"
import { KeyRoundIcon, TrendingDownIcon, TrendingUpIcon } from "lucide-react"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import {
  ChartContainer,
  ChartTooltip,
  ChartTooltipContent,
  ChartLegend,
  ChartLegendContent,
  type ChartConfig,
} from "@/components/ui/chart"
import { adminApi } from "@/lib/api"
import {
  deliveryTimeSeries,
  DELIVERY_SERIES_COLORS,
  RISK_BOUNCE_RATE_PCT,
  RISK_COMPLAINT_RATE_PCT,
  serverApiP1,
  type DeliveryPoint,
} from "@/lib/api-p1"
import { firstApiCredentialKey } from "@/lib/api-extras"

const RANGES = [
  { value: 7, label: "7d" },
  { value: 30, label: "30d" },
  { value: 90, label: "90d" },
] as const

const chartConfig = {
  delivered: { label: "Delivered", theme: DELIVERY_SERIES_COLORS.delivered },
  pending: { label: "In flight", theme: DELIVERY_SERIES_COLORS.pending },
  bounced: { label: "Bounced", theme: DELIVERY_SERIES_COLORS.bounced },
} satisfies ChartConfig

/// A pill segmented control for the 7d/30d/90d range.
function RangeControl({
  value,
  onChange,
}: {
  value: number
  onChange: (value: number) => void
}) {
  return (
    <div className="inline-flex rounded-md border p-0.5">
      {RANGES.map((range) => (
        <button
          key={range.value}
          type="button"
          onClick={() => onChange(range.value)}
          className={`rounded px-2.5 py-1 text-xs font-medium transition-colors ${
            value === range.value
              ? "bg-primary text-primary-foreground"
              : "text-muted-foreground hover:text-foreground"
          }`}
        >
          {range.label}
        </button>
      ))}
    </div>
  )
}

function pct(part: number, whole: number): number | null {
  return whole > 0 ? (part / whole) * 100 : null
}

function formatPct(value: number | null): string {
  return value == null ? "—" : `${value.toFixed(value < 10 ? 2 : 1)}%`
}

/// One KPI card: big value + optional trend arrow + a tiny sparkline over
/// the series (or "—" when there is nothing to draw).
function KpiCard({
  label,
  value,
  hint,
  spark,
  sparkKind,
  invertTrend,
}: {
  label: string
  value: string
  hint: string
  spark: number[]
  sparkKind: "area" | "line"
  /** When true, a rising line is "bad" (bounce/complaint rates). */
  invertTrend?: boolean
}) {
  const points = spark.map((v, i) => ({ i, v }))
  const first = spark.find((v) => v > 0) ?? spark[0] ?? 0
  const last = spark[spark.length - 1] ?? 0
  const delta = last - first
  const hasTrend = spark.length >= 2 && delta !== 0
  const up = delta > 0
  const good = invertTrend ? !up : up
  const TrendIcon = up ? TrendingUpIcon : TrendingDownIcon

  return (
    <Card className="gap-2 py-5">
      <CardHeader>
        <CardTitle className="text-xs font-medium text-muted-foreground">{label}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-2">
        <div className="flex items-end justify-between gap-2">
          <span className="text-2xl font-semibold tabular-nums">{value}</span>
          {hasTrend && (
            <span
              className={`flex items-center gap-0.5 text-xs ${
                good ? "text-green-600 dark:text-green-400" : "text-red-600 dark:text-red-400"
              }`}
            >
              <TrendIcon className="size-3.5" />
            </span>
          )}
        </div>
        {spark.some((v) => v > 0) ? (
          <ChartContainer config={chartConfig} className="h-8 w-full">
            {sparkKind === "area" ? (
              <AreaChart data={points} margin={{ top: 2, bottom: 2, left: 0, right: 0 }}>
                <Area
                  dataKey="v"
                  type="monotone"
                  fill="var(--color-delivered)"
                  fillOpacity={0.15}
                  stroke="var(--color-delivered)"
                  strokeWidth={1.5}
                  isAnimationActive={false}
                  dot={false}
                />
              </AreaChart>
            ) : (
              <LineChart data={points} margin={{ top: 2, bottom: 2, left: 0, right: 0 }}>
                <Line
                  dataKey="v"
                  type="monotone"
                  stroke="var(--color-bounced)"
                  strokeWidth={1.5}
                  isAnimationActive={false}
                  dot={false}
                />
              </LineChart>
            )}
          </ChartContainer>
        ) : (
          <div className="flex h-8 items-center text-xs text-muted-foreground">—</div>
        )}
        <p className="text-xs text-muted-foreground">{hint}</p>
      </CardContent>
    </Card>
  )
}

/// The big stacked delivery chart with a range control.
function DeliveryChart({ series }: { series: DeliveryPoint[] }) {
  return (
    <ChartContainer config={chartConfig} className="aspect-auto h-[280px] w-full">
      <AreaChart data={series} margin={{ left: 4, right: 8, top: 8 }}>
        <CartesianGrid vertical={false} />
        <XAxis dataKey="label" tickLine={false} axisLine={false} tickMargin={8} minTickGap={24} />
        <YAxis tickLine={false} axisLine={false} width={32} allowDecimals={false} />
        <ChartTooltip content={<ChartTooltipContent />} />
        <ChartLegend content={<ChartLegendContent />} />
        <defs>
          {(["delivered", "pending", "bounced"] as const).map((key) => (
            <linearGradient key={key} id={`fill-${key}`} x1="0" y1="0" x2="0" y2="1">
              <stop offset="5%" stopColor={`var(--color-${key})`} stopOpacity={0.6} />
              <stop offset="95%" stopColor={`var(--color-${key})`} stopOpacity={0.08} />
            </linearGradient>
          ))}
        </defs>
        <Area
          dataKey="delivered"
          type="monotone"
          stackId="a"
          fill="url(#fill-delivered)"
          stroke="var(--color-delivered)"
          strokeWidth={2}
        />
        <Area
          dataKey="pending"
          type="monotone"
          stackId="a"
          fill="url(#fill-pending)"
          stroke="var(--color-pending)"
          strokeWidth={2}
        />
        <Area
          dataKey="bounced"
          type="monotone"
          stackId="a"
          fill="url(#fill-bounced)"
          stroke="var(--color-bounced)"
          strokeWidth={2}
        />
      </AreaChart>
    </ChartContainer>
  )
}

/// A single-series rate chart (bounce / complaint) with a dashed RISK
/// threshold line — the deliverability early-warning.
function RateChart({
  series,
  dataKey,
  color,
  risk,
  label,
}: {
  series: { label: string; value: number | null }[]
  dataKey: string
  color: string
  risk: number
  label: string
}) {
  const config = { value: { label, color } } satisfies ChartConfig
  const max = Math.max(risk * 1.4, ...series.map((p) => p.value ?? 0))
  return (
    <ChartContainer config={config} className="aspect-auto h-[180px] w-full">
      <LineChart data={series} margin={{ left: 4, right: 8, top: 8 }}>
        <CartesianGrid vertical={false} />
        <XAxis dataKey="label" tickLine={false} axisLine={false} tickMargin={8} minTickGap={24} />
        <YAxis
          tickLine={false}
          axisLine={false}
          width={40}
          domain={[0, Math.ceil(max * 100) / 100]}
          tickFormatter={(v: number) => `${v}%`}
        />
        <ChartTooltip
          content={
            <ChartTooltipContent
              formatter={(value) => (
                <span className="font-mono tabular-nums">
                  {typeof value === "number" ? `${value.toFixed(2)}%` : String(value)}
                </span>
              )}
            />
          }
        />
        <ReferenceLine
          y={risk}
          stroke="var(--color-value)"
          strokeDasharray="5 4"
          strokeOpacity={0.7}
          label={{
            value: `RISK ${risk}%`,
            position: "insideTopRight",
            fontSize: 10,
            fill: "var(--color-value)",
          }}
        />
        <Line
          dataKey={dataKey}
          type="monotone"
          stroke="var(--color-value)"
          strokeWidth={2}
          dot={false}
          connectNulls
        />
      </LineChart>
    </ChartContainer>
  )
}

/// The dashboard body once a usable API credential is known.
function Dashboard({ apiKey }: { apiKey: string }) {
  const [days, setDays] = useState<number>(30)

  const series = useQuery({
    queryKey: ["p1-delivery-series", apiKey, days],
    queryFn: () => deliveryTimeSeries(apiKey, days),
  })
  // 30-day KPI totals: one exact windowed call, independent of the chart range.
  const kpi = useQuery({
    queryKey: ["p1-kpi-30d", apiKey],
    queryFn: () => {
      const to = new Date()
      const from = new Date(to.getTime() - 30 * 86_400_000)
      return serverApiP1(apiKey).statsWindow(from, to).then((r) => r.stats)
    },
  })

  const points = useMemo(() => series.data ?? [], [series.data])
  const sentSpark = points.map((p) => p.sent)
  const deliverySpark = points.map((p) => p.delivered)
  const bounceRateSpark = points.map((p) => p.bounceRate ?? 0)

  const s = kpi.data
  const deliveryRate = s ? pct(s.sent, s.outgoing) : null
  const bounceRate = s ? pct(s.bounced, s.outgoing) : null

  const rateSeries = useMemo(
    () => points.map((p) => ({ label: p.label, value: p.bounceRate })),
    [points],
  )

  return (
    <section className="mb-8 space-y-4">
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
        <KpiCard
          label="Sent (30d)"
          value={s ? s.outgoing.toLocaleString() : "—"}
          hint="Outgoing messages, last 30 days"
          spark={sentSpark}
          sparkKind="area"
        />
        <KpiCard
          label="Delivery rate"
          value={formatPct(deliveryRate)}
          hint="Delivered ÷ sent, last 30 days"
          spark={deliverySpark}
          sparkKind="area"
        />
        <KpiCard
          label="Bounce rate"
          value={formatPct(bounceRate)}
          hint={`Warning threshold ${RISK_BOUNCE_RATE_PCT}%`}
          spark={bounceRateSpark}
          sparkKind="line"
          invertTrend
        />
        <KpiCard
          label="Complaints"
          value="—"
          hint="Feedback-loop complaints (not yet tracked)"
          spark={[]}
          sparkKind="line"
          invertTrend
        />
      </div>

      <Card>
        <CardHeader className="flex flex-row items-center justify-between">
          <CardTitle className="text-base">Deliveries over time</CardTitle>
          <RangeControl value={days} onChange={setDays} />
        </CardHeader>
        <CardContent>
          {series.isLoading ? (
            <div className="flex h-[280px] items-center justify-center text-sm text-muted-foreground">
              Loading…
            </div>
          ) : points.every((p) => p.sent === 0) ? (
            <div className="flex h-[280px] flex-col items-center justify-center gap-1 text-center text-sm text-muted-foreground">
              <p>No messages in this window yet.</p>
              <p className="text-xs">Send your first message to see deliveries here.</p>
            </div>
          ) : (
            <DeliveryChart series={points} />
          )}
        </CardContent>
      </Card>

      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Bounce rate</CardTitle>
          </CardHeader>
          <CardContent>
            <RateChart
              series={rateSeries}
              dataKey="value"
              color={DELIVERY_SERIES_COLORS.bounced.light}
              risk={RISK_BOUNCE_RATE_PCT}
              label="Bounce rate"
            />
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Complaint rate</CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex h-[180px] flex-col items-center justify-center gap-1 text-center text-sm text-muted-foreground">
              <p>Complaint tracking is not available yet.</p>
              <p className="text-xs">Warning threshold {RISK_COMPLAINT_RATE_PCT}%.</p>
            </div>
          </CardContent>
        </Card>
      </div>
    </section>
  )
}

/// Org-home dashboard: resolves the first server + its first API
/// credential, then renders the KPI/chart dashboard. Renders nothing
/// (so the Servers list/onboarding shows through) when there is no
/// server; a small hint when a server exists but has no API credential.
export function OrgDashboard({ org }: { org: string }) {
  const servers = useQuery({
    queryKey: ["servers", org],
    queryFn: () => adminApi.servers(org).list(),
  })
  const server = servers.data?.servers[0]

  const credential = useQuery({
    queryKey: ["p1-dashboard-key", org, server?.permalink],
    queryFn: () => firstApiCredentialKey(org, server!.permalink),
    enabled: !!server,
  })

  if (!server) return null

  if (credential.isSuccess && !credential.data) {
    return (
      <Card className="mb-8">
        <CardContent className="flex flex-col items-center gap-3 py-10 text-center">
          <div className="flex size-12 items-center justify-center rounded-full bg-muted">
            <KeyRoundIcon className="size-5 text-muted-foreground" />
          </div>
          <div className="space-y-1">
            <h3 className="text-sm font-medium">Connect an API credential to see analytics</h3>
            <p className="mx-auto max-w-sm text-sm text-muted-foreground">
              Delivery stats come from “{server.name}”’s own messaging API. Create an API
              credential to light up the dashboard.
            </p>
          </div>
          <Button asChild size="sm">
            <Link href={`/orgs/${org}/servers/${server.permalink}/credentials`}>
              Create API credential
            </Link>
          </Button>
        </CardContent>
      </Card>
    )
  }

  if (!credential.data) return null
  return <Dashboard apiKey={credential.data} />
}
