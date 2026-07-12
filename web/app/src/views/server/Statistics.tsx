"use client"

// Statistics (masterplan §4.10): sentence-shaped KPIs with denominators
// and colored numbers, a clickable bounce-cause breakdown (hard / soft /
// undetermined as a proportion bar), and an opened / clicked engagement
// donut with absolute numbers — all over a real windowed /stats call.
// Rankings (top clients / platforms) are deliberately omitted: the stats
// API exposes no per-client data and we never fabricate it.

import { useMemo, useState, type CSSProperties } from "react"
import { useQuery } from "@tanstack/react-query"
import { Cell, Pie, PieChart } from "recharts"
import { BarChart3Icon } from "lucide-react"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
import { EmptyState } from "@/components/empty-state"
import { useMessagingApiP1 } from "@/views/server/Messaging"
import {
  bounceBreakdown,
  formatPct,
  ratePct,
  STAT_COLORS,
  type BounceSlice,
} from "@/lib/api-p4"

const DAY_MS = 86_400_000

const PRESETS = [
  { value: "7", label: "7 days", days: 7 },
  { value: "30", label: "30 days", days: 30 },
  { value: "90", label: "90 days", days: 90 },
  { value: "custom", label: "Custom", days: 0 },
] as const

type Preset = (typeof PRESETS)[number]["value"]

/// The 7d / 30d / 90d + Custom range control with an inline date range.
function RangeControl({
  preset,
  onPreset,
  from,
  to,
  onFrom,
  onTo,
}: {
  preset: Preset
  onPreset: (p: Preset) => void
  from: string
  to: string
  onFrom: (v: string) => void
  onTo: (v: string) => void
}) {
  return (
    <div className="flex flex-wrap items-center gap-2">
      <div className="inline-flex rounded-md border p-0.5">
        {PRESETS.map((p) => (
          <button
            key={p.value}
            type="button"
            onClick={() => onPreset(p.value)}
            className={`rounded px-2.5 py-1 text-xs font-medium transition-colors ${
              preset === p.value
                ? "bg-primary text-primary-foreground"
                : "text-muted-foreground hover:text-foreground"
            }`}
          >
            {p.label}
          </button>
        ))}
      </div>
      {preset === "custom" && (
        <div className="flex items-center gap-2">
          <Input
            type="date"
            value={from}
            max={to}
            onChange={(e) => onFrom(e.target.value)}
            className="h-8 w-auto"
          />
          <span className="text-xs text-muted-foreground">to</span>
          <Input
            type="date"
            value={to}
            min={from}
            onChange={(e) => onTo(e.target.value)}
            className="h-8 w-auto"
          />
        </div>
      )}
    </div>
  )
}

/// A colored number inside a KPI sentence.
function Num({ value, tone }: { value: string; tone?: "good" | "bad" | "warn" | "info" }) {
  const cls =
    tone === "good"
      ? "text-green-600 dark:text-green-400"
      : tone === "bad"
        ? "text-red-600 dark:text-red-400"
        : tone === "warn"
          ? "text-amber-600 dark:text-amber-400"
          : tone === "info"
            ? "text-sky-600 dark:text-sky-400"
            : "text-foreground"
  return <span className={`font-semibold tabular-nums ${cls}`}>{value}</span>
}

/// The clickable bounce-cause proportion bar. Selecting a slice reveals
/// its share and cause below — no fabricated drill-downs.
function BounceBar({ slices, total }: { slices: BounceSlice[]; total: number }) {
  const [active, setActive] = useState<BounceSlice["key"] | null>(null)
  if (total === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        No bounces in this window. Your bounce rate is clean.
      </p>
    )
  }
  const selected = slices.find((s) => s.key === active) ?? null
  return (
    <div className="space-y-3">
      <div className="flex h-4 w-full overflow-hidden rounded-full">
        {slices.map((slice) => {
          const width = (slice.count / total) * 100
          if (width === 0) return null
          return (
            <button
              key={slice.key}
              type="button"
              onClick={() => setActive((a) => (a === slice.key ? null : slice.key))}
              title={`${slice.label}: ${slice.count}`}
              aria-label={`${slice.label}: ${slice.count}`}
              className="h-full transition-opacity hover:opacity-80"
              style={{
                width: `${width}%`,
                backgroundColor: `var(--stat-${slice.key})`,
                opacity: active && active !== slice.key ? 0.4 : 1,
              }}
            />
          )
        })}
      </div>
      <div className="flex flex-wrap gap-3">
        {slices.map((slice) => (
          <button
            key={slice.key}
            type="button"
            onClick={() => setActive((a) => (a === slice.key ? null : slice.key))}
            className={`flex items-center gap-1.5 text-xs ${
              active === slice.key ? "font-medium" : "text-muted-foreground"
            }`}
          >
            <span
              className="size-2.5 rounded-sm"
              style={{ backgroundColor: `var(--stat-${slice.key})` }}
            />
            {slice.label}
            <span className="tabular-nums">
              {slice.count} ({formatPct(ratePct(slice.count, total))})
            </span>
          </button>
        ))}
      </div>
      {selected && (
        <p className="rounded-md border bg-muted/40 p-2 text-xs text-muted-foreground">
          {BOUNCE_HELP[selected.key]}
        </p>
      )}
    </div>
  )
}

const BOUNCE_HELP: Record<BounceSlice["key"], string> = {
  hard: "Permanent failures. The address does not exist or rejected mail outright. These recipients are suppressed to protect your reputation.",
  soft: "Transient failures: a full mailbox, greylisting or a temporary server issue. The worker retries these before giving up.",
  undetermined:
    "The receiving server did not give a clear reason. Watch for a pattern before acting.",
}

const donutConfig = {
  clicked: { label: "Clicked", theme: STAT_COLORS.clicked },
  opened: { label: "Opened", theme: STAT_COLORS.opened },
  neither: { label: "Not opened", theme: STAT_COLORS.neither },
} satisfies ChartConfig

/// Engagement donut over the delivered mail of the window. Segments sum to
/// delivered: clicked ⊂ opened ⊂ delivered, shown as disjoint bands.
function EngagementDonut({
  delivered,
  uniqueOpens,
  uniqueClicks,
}: {
  delivered: number
  uniqueOpens: number
  uniqueClicks: number
}) {
  const clicked = Math.min(uniqueClicks, delivered)
  const openedOnly = Math.max(0, Math.min(uniqueOpens, delivered) - clicked)
  const neither = Math.max(0, delivered - clicked - openedOnly)
  const data = [
    { key: "clicked", label: "Clicked", value: clicked },
    { key: "opened", label: "Opened", value: openedOnly },
    { key: "neither", label: "Not opened", value: neither },
  ].filter((d) => d.value > 0)

  if (delivered === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        Nothing delivered in this window yet.
      </p>
    )
  }

  return (
    <div className="flex flex-wrap items-center gap-6">
      <ChartContainer config={donutConfig} className="aspect-square h-40">
        <PieChart>
          <Pie
            data={data}
            dataKey="value"
            nameKey="label"
            innerRadius="60%"
            outerRadius="100%"
            strokeWidth={2}
            isAnimationActive={false}
          >
            {data.map((d) => (
              <Cell key={d.key} fill={`var(--color-${d.key})`} />
            ))}
          </Pie>
        </PieChart>
      </ChartContainer>
      <dl className="space-y-1.5 text-sm">
        <div className="flex items-center gap-2">
          <span className="size-2.5 rounded-sm" style={{ backgroundColor: "var(--stat-clicked)" }} />
          <dt className="w-24 text-muted-foreground">Clicked</dt>
          <dd className="tabular-nums font-medium">{clicked}</dd>
        </div>
        <div className="flex items-center gap-2">
          <span className="size-2.5 rounded-sm" style={{ backgroundColor: "var(--stat-opened)" }} />
          <dt className="w-24 text-muted-foreground">Opened</dt>
          <dd className="tabular-nums font-medium">{Math.min(uniqueOpens, delivered)}</dd>
        </div>
        <div className="flex items-center gap-2">
          <span className="size-2.5 rounded-sm" style={{ backgroundColor: "var(--stat-neither)" }} />
          <dt className="w-24 text-muted-foreground">Not opened</dt>
          <dd className="tabular-nums font-medium">{neither}</dd>
        </div>
      </dl>
    </div>
  )
}

// CSS custom properties for the bounce bar colors (light values; dark
// handled inline via the same tokens the chart config uses).
const statVars = {
  "--stat-hard": STAT_COLORS.hard.light,
  "--stat-soft": STAT_COLORS.soft.light,
  "--stat-undetermined": STAT_COLORS.undetermined.light,
  "--stat-clicked": STAT_COLORS.clicked.light,
  "--stat-opened": STAT_COLORS.opened.light,
  "--stat-neither": STAT_COLORS.neither.light,
} as CSSProperties

export function Statistics() {
  const p1 = useMessagingApiP1()
  const today = useMemo(() => new Date(), [])
  const [preset, setPreset] = useState<Preset>("30")
  const [customFrom, setCustomFrom] = useState(
    () => new Date(today.getTime() - 30 * DAY_MS).toISOString().slice(0, 10),
  )
  const [customTo, setCustomTo] = useState(() => today.toISOString().slice(0, 10))

  const { from, to } = useMemo(() => {
    if (preset === "custom") {
      const f = new Date(`${customFrom}T00:00:00`)
      const t = new Date(`${customTo}T23:59:59`)
      return { from: f, to: t }
    }
    const days = PRESETS.find((p) => p.value === preset)?.days ?? 30
    const t = new Date()
    return { from: new Date(t.getTime() - days * DAY_MS), to: t }
  }, [preset, customFrom, customTo])

  const stats = useQuery({
    queryKey: ["p4-stats", from.toISOString(), to.toISOString()],
    queryFn: () => p1.statsWindow(from, to).then((r) => r.stats),
  })

  const s = stats.data
  const breakdown = s ? bounceBreakdown(s) : null

  return (
    <div className="space-y-6" style={statVars}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-base font-semibold">Statistics</h2>
        <RangeControl
          preset={preset}
          onPreset={setPreset}
          from={customFrom}
          to={customTo}
          onFrom={setCustomFrom}
          onTo={setCustomTo}
        />
      </div>

      {stats.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : !s ? (
        <EmptyState
          icon={BarChart3Icon}
          title="No statistics available"
          description="Statistics appear once this server has processed mail in the selected window."
        />
      ) : s.outgoing === 0 ? (
        <EmptyState
          icon={BarChart3Icon}
          title="No mail sent in this window"
          description="Widen the range or send your first email to see delivery statistics here."
        />
      ) : (
        <>
          {/* Sentence KPIs with denominators + colored numbers */}
          <Card>
            <CardContent className="space-y-3 py-5 text-sm leading-relaxed">
              <p>
                You sent <Num value={s.outgoing.toLocaleString()} /> email
                {s.outgoing === 1 ? "" : "s"}, of which{" "}
                <Num
                  value={formatPct(ratePct(s.bounced, s.outgoing))}
                  tone={ratePct(s.bounced, s.outgoing)! >= 4 ? "bad" : "warn"}
                />{" "}
                bounced (<Num value={s.bounced.toLocaleString()} tone="bad" /> total).
              </p>
              <p>
                <Num value={s.sent.toLocaleString()} tone="good" /> were delivered
                {" "}
                (<Num value={formatPct(ratePct(s.sent, s.outgoing))} tone="good" /> delivery
                rate), and <Num value={s.held.toLocaleString()} tone="warn" /> were held for
                review.
              </p>
              <p>
                Recipients opened your mail{" "}
                <Num value={s.opens.toLocaleString()} tone="info" /> time
                {s.opens === 1 ? "" : "s"} (
                <Num value={s.unique_opens.toLocaleString()} tone="info" /> unique,{" "}
                <Num value={formatPct(ratePct(s.unique_opens, s.sent))} tone="info" /> open
                rate) and clicked <Num value={s.clicks.toLocaleString()} tone="info" /> time
                {s.clicks === 1 ? "" : "s"} (
                <Num value={s.unique_clicks.toLocaleString()} tone="info" /> unique,{" "}
                <Num value={formatPct(ratePct(s.unique_clicks, s.sent))} tone="info" /> click
                rate).
              </p>
            </CardContent>
          </Card>

          <div className="grid gap-6 lg:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle className="text-sm">Bounce causes</CardTitle>
              </CardHeader>
              <CardContent>
                {breakdown && <BounceBar slices={breakdown.slices} total={breakdown.total} />}
              </CardContent>
            </Card>

            <Card>
              <CardHeader>
                <CardTitle className="text-sm">Engagement</CardTitle>
              </CardHeader>
              <CardContent>
                <EngagementDonut
                  delivered={s.sent}
                  uniqueOpens={s.unique_opens}
                  uniqueClicks={s.unique_clicks}
                />
              </CardContent>
            </Card>
          </div>
        </>
      )}
    </div>
  )
}
