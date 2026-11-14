"use client"

// Instance admin: what Google Postmaster Tools, Microsoft SNDS and the
// complaint feedback loops report about this installation's mail.

import { useQuery } from "@tanstack/react-query"
import { PageHeader } from "@/components/shared"
import { InfrastructureNav } from "./infrastructure/InfrastructureNav"
import { Page } from "@/components/page"
import { StatusPill } from "@/components/status-pill"
import { Badge } from "@/components/ui/badge"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { DataTable, type DataTableColumn } from "@/components/ui/data-table"
import { adminApi, type FeedbackSnapshot, type FeedbackSource } from "@/lib/api"
import { relativeTime } from "@/lib/api-p1"
import {
  age,
  formatMetric,
  PROVIDER_LABELS,
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
}

function SnapshotTable({ source }: { source: FeedbackSource }) {
  const columns: DataTableColumn<FeedbackSnapshot>[] = [
    {
      id: "subject",
      header: source.source === "fbl" ? "Provider" : source.source === "snds" ? "IP" : "Domain",
      accessorFn: (r) => r.subject,
      cell: ({ row }) => (
        <span className="block truncate font-mono text-xs font-medium">
          {row.original.subject_kind === "provider"
            ? (PROVIDER_LABELS[row.original.subject] ?? row.original.subject)
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
      </div>
    </Page>
  )
}
