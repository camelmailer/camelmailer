"use client"

// Instance admin: what Google Postmaster Tools, Microsoft SNDS and the
// complaint feedback loops report about this installation's mail.

import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { PlayIcon } from "lucide-react"
import { toast } from "sonner"
import { PageHeader } from "@/components/shared"
import { InfrastructureNav } from "./infrastructure/InfrastructureNav"
import { Page } from "@/components/page"
import { StatusPill } from "@/components/status-pill"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { DataTable, type DataTableColumn } from "@/components/ui/data-table"
import { Button } from "@/components/ui/button"
import {
  adminApi,
  ApiError,
  type FeedbackSnapshot,
  type FeedbackSource,
  type PlacementTest,
} from "@/lib/api"
import { relativeTime } from "@/lib/api-p1"
import {
  age,
  authSummary,
  formatMetric,
  placementTone,
  providerLabel,
  SOURCE_LABELS,
  sourceState,
  VERDICT_LABELS,
  verdictTone,
  worstVerdict,
} from "@/views/admin/feedback"

const STATE_PILLS: Record<
  ReturnType<typeof sourceState>,
  { label: string; tone: "green" | "amber" | "red" | "gray" }
> = {
  off: { label: "Not configured", tone: "gray" },
  failing: { label: "Fetch failing", tone: "red" },
  stale: { label: "Stale", tone: "amber" },
  waiting: { label: "Waiting for data", tone: "gray" },
  fresh: { label: "Up to date", tone: "green" },
}

// The values worth a column, per source; everything else is in the detail.
const METRIC_COLUMNS: Record<FeedbackSource["source"], [string, string][]> = {
  postmaster_tools: [
    ["spam_rate", "Spam rate"],
    ["dmarc_success_rate", "DMARC pass"],
    ["reject_rate", "Rejected"],
  ],
  snds: [
    ["filter_result", "Filter"],
    ["complaint_rate", "Complaints"],
    ["trap_hits", "Trap hits"],
  ],
  fbl: [["complaints", "Complaints"]],
  placement: [
    ["seed", "Seed"],
    ["folder", "Folder"],
  ],
}

function SnapshotTable({ source }: { source: FeedbackSource }) {
  const columns: DataTableColumn<FeedbackSnapshot>[] = [
    {
      id: "subject",
      header:
        source.source === "fbl"
          ? "Provider"
          : source.source === "postmaster_tools"
            ? "Domain"
            : "IP",
      accessorFn: (r) => r.subject,
      cell: ({ row }) => (
        <span className="block truncate font-mono text-xs font-medium">
          {row.original.subject_kind === "provider"
            ? providerLabel(row.original.subject)
            : row.original.subject}
        </span>
      ),
    },
    {
      id: "verdict",
      header: "Verdict",
      accessorFn: (r) => r.verdict,
      cell: ({ row }) => (
        <StatusPill
          status={VERDICT_LABELS[row.original.verdict]}
          tone={verdictTone(row.original.verdict)}
        />
      ),
    },
    ...METRIC_COLUMNS[source.source].map(
      ([key, label]): DataTableColumn<FeedbackSnapshot> => ({
        id: key,
        header: label,
        accessorFn: (r) => formatMetric(key, r.metrics[key]),
        meta: { align: "right" },
        cell: ({ row }) => (
          <span className="inline-block min-w-16 tabular-nums">
            {formatMetric(key, row.original.metrics[key])}
          </span>
        ),
      }),
    ),
    {
      id: "detail",
      header: "Why",
      accessorFn: (r) => r.detail,
      cell: ({ row }) => (
        <span className="text-muted-foreground">{row.original.detail || "—"}</span>
      ),
    },
    {
      id: "data_date",
      header: "Day",
      accessorFn: (r) => r.data_date,
      cell: ({ row }) => <span className="tabular-nums">{row.original.data_date}</span>,
    },
  ]
  return (
    <DataTable
      columns={columns}
      data={source.snapshots}
      searchKeys={["subject"]}
      initialPageSize={10}
      filters={[
        {
          columnId: "verdict",
          label: "Verdict",
          options: (["bad", "warn", "ok", "unknown"] as const).map((v) => ({
            label: VERDICT_LABELS[v],
            value: v,
          })),
        },
      ]}
      emptyText={
        source.source === "fbl"
          ? "No complaints reported so far."
          : "No data yet. Providers publish once a day, and only for domains and IPs with enough volume."
      }
    />
  )
}

function SourceCard({ source }: { source: FeedbackSource }) {
  const labels = SOURCE_LABELS[source.source]
  const state = sourceState(source)
  const pill = STATE_PILLS[state]
  const worst = worstVerdict(source)
  const freshness = age(source.freshness_seconds)
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center gap-2 text-base">
          {labels.title}
          <Badge variant="outline">{labels.provider}</Badge>
          <StatusPill status={pill.label} tone={pill.tone} />
          {source.snapshots.length > 0 && worst !== "unknown" && (
            <StatusPill status={VERDICT_LABELS[worst]} tone={verdictTone(worst)} />
          )}
        </CardTitle>
        <p className="text-sm text-muted-foreground">
          {freshness ? `Newest data ${freshness} old.` : "No data received yet."}
          {source.status?.next_attempt_at && state !== "off" && source.source !== "fbl"
            ? ` Next fetch ${relativeTime(source.status.next_attempt_at)}.`
            : ""}
          {state === "off" &&
            " Add the credentials described in the provider feedback documentation to switch it on."}
        </p>
        {source.status?.last_error && (
          <p className="text-sm text-destructive">
            Last fetch failed
            {source.status.consecutive_failures > 1
              ? ` (${source.status.consecutive_failures} times in a row)`
              : ""}
            : {source.status.last_error}
          </p>
        )}
      </CardHeader>
      {state !== "off" && (
        <CardContent>
          <SnapshotTable source={source} />
        </CardContent>
      )}
    </Card>
  )
}

function PlacementCard() {
  const queryClient = useQueryClient()
  const tests = useQuery({
    queryKey: ["admin", "placement-tests"],
    queryFn: adminApi.deliverabilityFeedback.placementTests,
    refetchInterval: 60_000,
  })
  const run = useMutation({
    mutationFn: adminApi.deliverabilityFeedback.runPlacementTests,
    onSuccess: () => {
      toast.success("Placement round requested. A worker sends it within a minute.")
      void queryClient.invalidateQueries({ queryKey: ["admin", "placement-tests"] })
    },
    onError: (err) =>
      toast.error(
        err instanceof ApiError ? err.message : "The placement round could not be requested",
      ),
  })
  const columns: DataTableColumn<PlacementTest>[] = [
    {
      id: "ip",
      header: "IP",
      accessorFn: (r) => r.ip,
      filterFn: (row, _id, value) => row.original.ip === value,
      cell: ({ row }) => (
        <span className="block truncate font-mono text-xs font-medium">{row.original.ip}</span>
      ),
    },
    {
      id: "provider",
      header: "Provider",
      accessorFn: (r) => r.provider,
      filterFn: (row, _id, value) => row.original.provider === value,
      cell: ({ row }) => <span>{providerLabel(row.original.provider)}</span>,
    },
    {
      id: "seed_address",
      header: "Seed",
      accessorFn: (r) => r.seed_address,
      cell: ({ row }) => <span className="truncate">{row.original.seed_address}</span>,
    },
    {
      id: "status",
      header: "Result",
      accessorFn: (r) => r.status,
      filterFn: (row, _id, value) => row.original.status === value,
      cell: ({ row }) => {
        const pill = placementTone(row.original.status)
        return <StatusPill status={pill.label} tone={pill.tone} />
      },
    },
    {
      id: "auth",
      header: "Authentication",
      accessorFn: (r) => authSummary(r) ?? "",
      cell: ({ row }) => (
        <span className="text-muted-foreground">{authSummary(row.original) ?? "—"}</span>
      ),
    },
    {
      id: "received",
      header: "Seen as",
      accessorFn: (r) => r.helo ?? "",
      cell: ({ row }) => (
        <span className="font-mono text-xs text-muted-foreground" title={row.original.detail}>
          {row.original.helo || row.original.received_ip
            ? `${row.original.helo ?? "?"} [${row.original.received_ip ?? "?"}]`
            : row.original.detail || "—"}
        </span>
      ),
    },
    {
      id: "checked_at",
      header: "Checked",
      accessorFn: (r) => r.checked_at ?? r.created_at,
      cell: ({ row }) => (
        <span className="text-muted-foreground" title={row.original.checked_at ?? row.original.created_at}>
          {relativeTime(row.original.checked_at ?? row.original.created_at)}
        </span>
      ),
    },
  ]
  const history = tests.data?.placement_tests ?? []
  const distinct = (values: string[]) => Array.from(new Set(values)).sort()
  const configured = tests.data?.configured ?? false
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center gap-2 text-base">
          Seed placement history
          <Button
            size="sm"
            variant="outline"
            className="ml-auto"
            disabled={!configured || run.isPending}
            onClick={() => run.mutate()}
          >
            <PlayIcon />
            Run a round now
          </Button>
        </CardTitle>
        <p className="text-sm text-muted-foreground">
          One test message from every sending IP to each of your seed mailboxes, then a read-only
          look into the inbox and the spam folder. Nothing in the seed mailboxes is opened, clicked,
          answered or moved. Every result is kept; the warm-up reads the worst result of the last
          24 hours per IP and provider.
          {!configured && " Configure seeds in deliverability_feedback.placement to use it."}
          {tests.data?.last_round &&
            ` Last round requested ${relativeTime(tests.data.last_round.requested_at)}.`}
        </p>
      </CardHeader>
      {configured && (
        <CardContent>
          <DataTable
            columns={columns}
            data={history}
            loading={tests.isLoading}
            searchKeys={["ip", "seed_address"]}
            initialPageSize={20}
            filters={[
              {
                columnId: "provider",
                label: "Provider",
                options: distinct(history.map((t) => t.provider)).map((p) => ({
                  label: providerLabel(p),
                  value: p,
                })),
              },
              {
                columnId: "ip",
                label: "IP",
                options: distinct(history.map((t) => t.ip)).map((ip) => ({ label: ip, value: ip })),
              },
              {
                columnId: "status",
                label: "Result",
                options: (["inbox", "spam", "missing", "pending", "error"] as const).map((v) => ({
                  label: placementTone(v).label,
                  value: v,
                })),
              },
            ]}
            emptyText="No placement tests yet. Run a round to start."
          />
        </CardContent>
      )}
    </Card>
  )
}

export default function ProviderFeedback() {
  const feedback = useQuery({
    queryKey: ["admin", "deliverability-feedback"],
    queryFn: adminApi.deliverabilityFeedback.list,
    refetchInterval: 60_000,
  })
  return (
    <Page
      variant="scroll"
      header={
        <>
          <PageHeader
            title="Provider feedback"
            description="What Gmail, Outlook.com and Yahoo report about the mail this installation sends: spam and complaint rates, filter results, and complaints from the feedback loops. Each source is fetched once its credentials are configured."
          />
          <InfrastructureNav />
        </>
      }
    >
      <div className="flex flex-col gap-6">
        {feedback.isLoading && <p className="text-sm text-muted-foreground">Loading…</p>}
        {feedback.error && (
          <p className="text-sm text-destructive">The provider feedback could not be loaded.</p>
        )}
        {feedback.data?.sources.map((source) => (
          <SourceCard key={source.source} source={source} />
        ))}
        <PlacementCard />
      </div>
    </Page>
  )
}
