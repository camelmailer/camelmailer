"use client"

// Instance admin: delivery counters per sending address and provider, and the
// adaptive warm-up (off by default): mode per address and pool, allowance and
// status per provider, the last brake, the estimate until warm and the
// decision log. Reads GET /api/v2/admin/ip_metrics and /ip_warmup; the mode
// selectors PATCH /ip_warmup/addresses/{id} and /ip_warmup/pools/{id}.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { toast } from "sonner"
import { PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { StatusPill } from "@/components/status-pill"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { DataTable, type DataTableColumn } from "@/components/ui/data-table"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { ApiError } from "@/lib/api"
import { relativeTime } from "@/lib/api-p1"
import { cn } from "@/lib/utils"
import {
  etaText,
  formatRate,
  lastBrake,
  MODE_LABELS,
  PROVIDER_LABELS,
  rateLevel,
  selectorValue,
  STATUS_LABELS,
  STATUS_TONE,
  usedPercent,
  warmupApi,
  warmupBody,
  type AddressMetrics,
  type WarmupAddress,
  type WarmupMode,
  type WarmupProviderState,
} from "@/lib/warmup"
import { InfrastructureNav } from "./InfrastructureNav"

const LEVEL_CLASS = {
  ok: "",
  warn: "text-amber-700 dark:text-amber-400",
  bad: "text-red-700 dark:text-red-400 font-semibold",
} as const

const MODES: WarmupMode[] = ["off", "slow", "normal", "fast", "custom"]

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

const metricColumns = (thresholds: Record<string, number>): DataTableColumn<AddressMetrics>[] => [
  {
    id: "source_ip",
    header: "Address",
    accessorFn: (r) => r.source_ip,
    cell: ({ row }) => (
      <div className="min-w-0">
        <div className="font-mono text-sm">
          {row.original.default_source ? "host default" : row.original.source_ip}
        </div>
        <div className="truncate text-xs text-muted-foreground">
          {row.original.hostname ?? (row.original.default_source ? "servers without a pool" : "")}
        </div>
      </div>
    ),
  },
  {
    id: "pool",
    header: "Pool",
    accessorFn: (r) => r.pool?.name ?? "",
    cell: ({ row }) => row.original.pool?.name ?? "—",
  },
  { id: "sent", header: "Sent", accessorFn: (r) => r.totals.sent },
  {
    id: "deferral_rate",
    header: "Deferred",
    accessorFn: (r) => r.totals.deferral_rate ?? 0,
    cell: ({ row }) => (
      <span
        className={LEVEL_CLASS[rateLevel(row.original.totals.deferral_rate, thresholds.deferral_rate_slow ?? 0.05, 0.2)]}
      >
        {formatRate(row.original.totals.deferral_rate)}
      </span>
    ),
  },
  {
    id: "bounce_rate",
    header: "Bounced",
    accessorFn: (r) => r.totals.bounce_rate ?? 0,
    cell: ({ row }) => (
      <span
        className={LEVEL_CLASS[rateLevel(row.original.totals.bounce_rate, 0.02, thresholds.bounce_rate_brake ?? 0.05)]}
      >
        {formatRate(row.original.totals.bounce_rate)}
      </span>
    ),
  },
  {
    id: "policy_blocks",
    header: "Policy refusals",
    accessorFn: (r) => r.totals.policy_blocks,
    cell: ({ row }) => (
      <span className={row.original.totals.policy_blocks > 0 ? LEVEL_CLASS.warn : ""}>
        {row.original.totals.policy_blocks}
      </span>
    ),
  },
  {
    id: "complaint_rate",
    header: "Complaints",
    accessorFn: (r) => r.totals.complaint_rate ?? 0,
    cell: ({ row }) => (
      <span
        className={
          LEVEL_CLASS[
            rateLevel(
              row.original.totals.complaint_rate,
              thresholds.complaint_rate_warn ?? 0.001,
              thresholds.complaint_rate_brake ?? 0.003,
            )
          ]
        }
      >
        {formatRate(row.original.totals.complaint_rate)}
      </span>
    ),
  },
  {
    id: "providers",
    header: "By provider (sent)",
    accessorFn: (r) => r.providers.length,
    cell: ({ row }) => (
      <span className="text-xs text-muted-foreground">
        {row.original.providers.map((p) => `${PROVIDER_LABELS[p.provider]} ${p.sent}`).join(" · ") || "—"}
      </span>
    ),
  },
]

function ProviderRow({ state }: { state: WarmupProviderState }) {
  const percent = usedPercent(state)
  return (
    <tr className="border-t align-top">
      <td className="py-2 pr-3 font-medium">{PROVIDER_LABELS[state.provider]}</td>
      <td className="py-2 pr-3">
        {state.status ? <StatusPill status={STATUS_LABELS[state.status]} tone={STATUS_TONE[state.status]} /> : "—"}
      </td>
      <td className="py-2 pr-3 tabular-nums">
        {state.status === "warm" ? "no limit" : (state.allowance?.toLocaleString() ?? "—")}
      </td>
      <td className="py-2 pr-3">
        <div className="flex items-center gap-2">
          <div className="h-1.5 w-20 overflow-hidden rounded bg-muted" aria-hidden>
            <div className="h-full bg-foreground/60" style={{ width: `${percent}%` }} />
          </div>
          <span className="tabular-nums text-xs text-muted-foreground">{state.used_today.toLocaleString()}</span>
        </div>
      </td>
      <td className="py-2 pr-3 text-xs">{etaText(state)}</td>
      <td className="py-2 text-xs text-muted-foreground">
        {state.paused && state.paused_until ? `paused until ${new Date(state.paused_until).toLocaleString()}. ` : ""}
        {state.last_reason ?? "—"}
      </td>
    </tr>
  )
}

function ModeSelector({
  value,
  onSave,
  pending,
  allowInherit,
  label,
}: {
  value: WarmupMode | "inherit"
  onSave: (mode: WarmupMode | "inherit", custom: { start: string; growth: string; cap: string }) => void
  pending: boolean
  allowInherit: boolean
  label: string
}) {
  const [mode, setMode] = useState<WarmupMode | "inherit">(value)
  const [custom, setCustom] = useState({ start: "100", growth: "1.5", cap: "50000" })
  const valid = warmupBody(mode, custom) !== null
  return (
    <div className="flex flex-wrap items-end gap-2">
      <Select
        value={mode}
        onValueChange={(next) => {
          const chosen = next as WarmupMode | "inherit"
          setMode(chosen)
          if (chosen !== "custom") onSave(chosen, custom)
        }}
        disabled={pending}
      >
        <SelectTrigger size="sm" aria-label={label} className="w-44">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {allowInherit && <SelectItem value="inherit">Inherit from pool</SelectItem>}
          {MODES.map((m) => (
            <SelectItem key={m} value={m}>
              {MODE_LABELS[m]}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      {mode === "custom" && (
        <>
          {(["start", "growth", "cap"] as const).map((field) => (
            <div key={field} className="flex flex-col gap-1">
              <Label className="text-xs capitalize">{field}</Label>
              <Input
                className="h-8 w-24"
                inputMode="decimal"
                value={custom[field]}
                onChange={(e) => setCustom({ ...custom, [field]: e.target.value })}
              />
            </div>
          ))}
          <Button size="sm" disabled={!valid || pending} onClick={() => onSave(mode, custom)}>
            Save
          </Button>
        </>
      )}
    </div>
  )
}

function AddressCard({ address, onChanged }: { address: WarmupAddress; onChanged: () => void }) {
  const setMode = useMutation({
    mutationFn: (args: { mode: WarmupMode | "inherit"; custom: { start: string; growth: string; cap: string } }) => {
      const body = warmupBody(args.mode, args.custom)
      if (!body) throw new Error("incomplete profile")
      return warmupApi.setAddress(address.ip_address_id, body)
    },
    onSuccess: () => {
      toast.success("Warm-up mode saved; the ramp continues from its current allowance")
      onChanged()
    },
    onError: (err) => errorToast(err, "Could not change the warm-up mode"),
  })
  const brake = lastBrake(address)
  return (
    <Card>
      <CardHeader className="flex flex-row flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <CardTitle className="font-mono text-base">{address.ipv4}</CardTitle>
          <p className="truncate text-sm text-muted-foreground">
            {address.hostname} · pool {address.pool.name}
          </p>
          <p className="mt-1 text-xs text-muted-foreground">
            Mode <span className="font-medium text-foreground">{MODE_LABELS[address.mode]}</span>{" "}
            ({address.mode_source === "address" ? "set on this address" : address.mode_source === "pool" ? "from the pool" : "default"})
            {address.profile &&
              ` · ${address.profile.start} per day, ×${address.profile.growth}, warm at ${address.profile.cap.toLocaleString()}`}
            {address.active && address.eta_days !== null && ` · about ${etaText({ eta_days: address.eta_days, status: null })} until warm`}
          </p>
        </div>
        <ModeSelector
          key={`${address.ip_address_id}-${selectorValue(address)}`}
          value={selectorValue(address)}
          pending={setMode.isPending}
          allowInherit
          label={`Warm-up mode of ${address.ipv4}`}
          onSave={(mode, custom) => setMode.mutate({ mode, custom })}
        />
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {!address.active ? (
          <p className="text-sm text-muted-foreground">
            Warm-up is off for this address: it sends without a limit, exactly as without warm-up.
          </p>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="text-left text-xs text-muted-foreground">
                  <th className="pb-1 pr-3 font-medium">Provider</th>
                  <th className="pb-1 pr-3 font-medium">Status</th>
                  <th className="pb-1 pr-3 font-medium">Allowance per day</th>
                  <th className="pb-1 pr-3 font-medium">Used today</th>
                  <th className="pb-1 pr-3 font-medium">Until warm</th>
                  <th className="pb-1 font-medium">Last decision</th>
                </tr>
              </thead>
              <tbody>
                {address.providers.map((p) => (
                  <ProviderRow key={p.provider} state={p} />
                ))}
              </tbody>
            </table>
          </div>
        )}
        {brake && (
          <Alert variant="destructive">
            <AlertTitle>
              Last brake: {PROVIDER_LABELS[brake.provider]}, {relativeTime(brake.at)}
            </AlertTitle>
            <AlertDescription>{brake.reason}</AlertDescription>
          </Alert>
        )}
        {address.recent_decisions.length > 0 && (
          <details>
            <summary className="cursor-pointer text-sm font-medium">Decision log</summary>
            <ul className="mt-2 flex flex-col gap-1 text-xs">
              {address.recent_decisions.map((entry, i) => (
                <li key={i} className="flex gap-2">
                  <span className="w-28 shrink-0 text-muted-foreground">{relativeTime(entry.created_at)}</span>
                  <span className="w-16 shrink-0 font-medium">{entry.decision}</span>
                  <span className="w-20 shrink-0">{entry.provider ? PROVIDER_LABELS[entry.provider] : ""}</span>
                  <span className="text-muted-foreground">{entry.reason}</span>
                </li>
              ))}
            </ul>
          </details>
        )}
      </CardContent>
    </Card>
  )
}

function PoolModes({ addresses, onChanged }: { addresses: WarmupAddress[]; onChanged: () => void }) {
  const pools = new Map<number, WarmupAddress["pool"]>()
  for (const a of addresses) pools.set(a.pool.id, a.pool)
  const setPool = useMutation({
    mutationFn: (args: { id: number; mode: WarmupMode | "inherit"; custom: { start: string; growth: string; cap: string } }) => {
      const body = warmupBody(args.mode, args.custom)
      if (!body) throw new Error("incomplete profile")
      return warmupApi.setPool(args.id, body)
    },
    onSuccess: () => {
      toast.success("Pool default saved")
      onChanged()
    },
    onError: (err) => errorToast(err, "Could not change the pool default"),
  })
  if (pools.size === 0) return null
  return (
    <Card>
      <CardHeader>
        <CardTitle>Pool defaults</CardTitle>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-sm text-muted-foreground">
          The mode every address of a pool uses unless it sets its own.
        </p>
        {[...pools.values()].map((pool) => (
          <div key={pool.id} className="flex flex-wrap items-center justify-between gap-3">
            <span className="font-medium">{pool.name}</span>
            <ModeSelector
              key={`${pool.id}-${pool.mode ?? "off"}`}
              value={pool.mode ?? "off"}
              pending={setPool.isPending}
              allowInherit={false}
              label={`Default warm-up mode of pool ${pool.name}`}
              onSave={(mode, custom) => setPool.mutate({ id: pool.id, mode: mode === "off" ? "inherit" : mode, custom })}
            />
          </div>
        ))}
      </CardContent>
    </Card>
  )
}

export default function IpMetricsWarmup() {
  const [days, setDays] = useState(7)
  const queryClient = useQueryClient()
  const metrics = useQuery({
    queryKey: ["admin", "ip_metrics", days],
    queryFn: () => warmupApi.metrics(days),
    refetchInterval: 60_000,
  })
  const warmup = useQuery({
    queryKey: ["admin", "ip_warmup"],
    queryFn: warmupApi.warmup,
    refetchInterval: 60_000,
  })
  const thresholds = warmup.data?.thresholds ?? {}
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["admin", "ip_warmup"] })
  const anyActive = warmup.data?.addresses.some((a) => a.active) ?? false

  return (
    <Page
      header={
        <>
          <PageHeader
            title="Infrastructure"
            description="Delivery counters per sending address and provider, and the adaptive warm-up."
          />
          <InfrastructureNav />
        </>
      }
    >
      <div className="flex flex-col gap-6">
        <section className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <h2 className="text-sm font-semibold">IP metrics</h2>
            <div className="flex gap-1" role="group" aria-label="Period">
              {[1, 7, 30, 90].map((d) => (
                <Button
                  key={d}
                  size="sm"
                  variant={d === days ? "secondary" : "ghost"}
                  onClick={() => setDays(d)}
                >
                  {d === 1 ? "Today" : `${d} days`}
                </Button>
              ))}
            </div>
          </div>
          {metrics.isError && (
            <Alert variant="destructive">
              <AlertTitle>The metrics could not be loaded</AlertTitle>
              <AlertDescription>{String(metrics.error)}</AlertDescription>
            </Alert>
          )}
          <DataTable
            columns={metricColumns(thresholds)}
            data={metrics.data?.addresses ?? []}
            loading={metrics.isLoading}
            searchKeys={["source_ip"]}
            emptyText="Nothing sent in this period. Every outbound result is counted here once it is recorded."
          />
          {metrics.data && metrics.data.pools.length > 0 && (
            <p className="text-xs text-muted-foreground">
              {metrics.data.pools
                .map((p) => `${p.name}: ${p.totals.sent} sent, ${formatRate(p.totals.deferral_rate)} deferred`)
                .join(" · ")}
            </p>
          )}
        </section>

        <section className="flex flex-col gap-3">
          <h2 className="text-sm font-semibold">Warm-up</h2>
          <p className={cn("text-sm text-muted-foreground")}>
            {anyActive
              ? "Each warming address gets a daily allowance per provider. It grows after a healthy, well-used day, holds when the volume is too low to judge, slows on rising deferrals and brakes on policy refusals, bounces or complaints. Volume beyond an allowance goes to another address of the pool with room."
              : "Warm-up is off for every address, so delivery is unchanged. Choose a mode for an address or a pool to start one."}
          </p>
          {warmup.isError && (
            <Alert variant="destructive">
              <AlertTitle>The warm-up state could not be loaded</AlertTitle>
              <AlertDescription>{String(warmup.error)}</AlertDescription>
            </Alert>
          )}
          {warmup.data && <PoolModes addresses={warmup.data.addresses} onChanged={refresh} />}
          {warmup.data?.addresses.map((address) => (
            <AddressCard key={address.ip_address_id} address={address} onChanged={refresh} />
          ))}
          {warmup.data && warmup.data.addresses.length === 0 && (
            <p className="text-sm text-muted-foreground">No IP pool has an address yet.</p>
          )}
        </section>
      </div>
    </Page>
  )
}
