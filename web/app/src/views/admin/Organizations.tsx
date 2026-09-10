"use client"

// Instance admin: every organization on this installation, with what it
// is actually sending. The point of this screen is early recognition —
// a tenant whose traffic, bounces or held mail changes shape should be
// visible here before anyone complains.

import { useState } from "react"
import Link from "next/link"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { TriangleAlertIcon, Trash2Icon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { DataTable } from "@/components/ui/data-table"
import { Skeleton } from "@/components/ui/skeleton"
import { StatusPill } from "@/components/status-pill"
import { relativeTime } from "@/lib/api-p1"
import { orgInitials } from "@/lib/api-extras"
import {
  adminApi,
  ApiError,
  type AdminOverview,
  type OrganizationVolume,
  type VolumeWindow,
} from "@/lib/api"
import {
  ACTIVITY_STATES,
  activity,
  bounceRate,
  count,
  FLAG_LABELS,
  flags,
  plural,
  windowDescription,
  windowLabel,
  WindowToggle,
} from "@/views/admin/volume"

/// The installation at a glance: a compact strip rather than tiles, so
/// the table below stays the main event.
function InstanceStrip({
  overview,
  range,
  loading,
}: {
  overview: AdminOverview | undefined
  range: VolumeWindow
  loading: boolean
}) {
  const instance = overview?.instance
  const counters = instance?.[range]
  const items: { label: string; value: string; hint?: string }[] = [
    { label: "Organizations", value: instance ? String(instance.organizations) : "—" },
    {
      label: "Servers",
      value: instance ? String(instance.servers) : "—",
      hint:
        instance && instance.suspended_servers > 0
          ? `${instance.suspended_servers} suspended`
          : undefined,
    },
    { label: "Users", value: instance ? String(instance.users) : "—" },
    { label: `Outgoing ${windowLabel(range)}`, value: counters ? count(counters.outgoing) : "—" },
    { label: `Bounce rate ${windowLabel(range)}`, value: counters ? bounceRate(counters) : "—" },
    { label: `Held ${windowLabel(range)}`, value: counters ? count(counters.held) : "—" },
  ]

  return (
    <div className="grid grid-cols-2 gap-x-6 gap-y-4 rounded-lg border bg-card px-4 py-3 sm:grid-cols-3 lg:grid-cols-6">
      {items.map((item) => (
        <div key={item.label} className="min-w-0">
          <div className="text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
            {item.label}
          </div>
          {loading ? (
            <Skeleton className="mt-1 h-6 w-14" />
          ) : (
            <div className="truncate text-lg font-semibold tabular-nums">{item.value}</div>
          )}
          {item.hint && <div className="text-xs text-amber-600">{item.hint}</div>}
        </div>
      ))}
    </div>
  )
}

/// The organizations carrying a flag, with the reason spelled out. Shown
/// only when there is something to say.
function AttentionStrip({ rows }: { rows: OrganizationVolume[] }) {
  const flagged = rows
    .map((row) => ({ row, found: flags(row) }))
    .filter((entry) => entry.found.length > 0)
  if (flagged.length === 0) return null

  const shown = flagged.slice(0, 5)
  return (
    <div className="rounded-lg border border-amber-500/40 bg-amber-500/5 px-4 py-3">
      <div className="flex items-center gap-2 text-sm font-medium">
        <TriangleAlertIcon className="size-4 text-amber-600" />
        {flagged.length === 1
          ? "One organization is worth a look"
          : `${flagged.length} organizations are worth a look`}
      </div>
      <ul className="mt-2 space-y-1 text-sm text-muted-foreground">
        {shown.map(({ row, found }) => (
          <li key={row.permalink} className="truncate">
            <Link
              href={`/admin/organizations/${row.permalink}`}
              className="font-medium text-foreground hover:underline"
            >
              {row.name}
            </Link>
            <span className="mx-1.5 text-muted-foreground/50">·</span>
            {found.map((flag) => flag.reason).join(", ")}
          </li>
        ))}
        {flagged.length > shown.length && (
          <li>{flagged.length - shown.length} more in the table below.</li>
        )}
      </ul>
    </div>
  )
}

export default function Organizations() {
  const queryClient = useQueryClient()
  const [range, setRange] = useState<VolumeWindow>("day")
  const [deleting, setDeleting] = useState<OrganizationVolume | null>(null)

  const overview = useQuery({
    queryKey: ["admin", "overview"],
    queryFn: adminApi.overview,
  })
  const remove = useMutation({
    mutationFn: (permalink: string) => adminApi.organizations.delete(permalink),
    onSuccess: (_data, permalink) => {
      toast.success(`Deleted ${permalink}.`)
      queryClient.invalidateQueries({ queryKey: ["admin", "overview"] })
      queryClient.invalidateQueries({ queryKey: ["admin", "organizations"] })
    },
    onError: (err) =>
      toast.error(
        err instanceof ApiError ? err.message : "Could not delete the organization",
      ),
  })

  const rows: OrganizationVolume[] = overview.data?.overview.organizations ?? []

  const columns: ColumnDef<OrganizationVolume>[] = [
    {
      id: "name",
      header: "Organization",
      accessorFn: (r) => r.name,
      cell: ({ row }) => (
        <Link
          href={`/admin/organizations/${row.original.permalink}`}
          className="flex items-center gap-2.5 font-medium transition-colors group-hover:text-primary"
        >
          <span className="flex size-6 shrink-0 items-center justify-center rounded-md bg-primary text-[10px] font-semibold text-primary-foreground">
            {orgInitials(row.original.name)}
          </span>
          <span className="min-w-0">
            <span className="block truncate">{row.original.name}</span>
            <span className="block truncate font-mono text-xs font-normal text-muted-foreground">
              {row.original.permalink}
            </span>
          </span>
        </Link>
      ),
    },
    {
      id: "status",
      header: "Status",
      enableSorting: false,
      accessorFn: (r) => activity(r).label,
      filterFn: (row, _id, value) => activity(row.original).label === value,
      cell: ({ row }) => {
        const state = activity(row.original)
        return <StatusPill status={state.label} tone={state.tone} />
      },
    },
    {
      id: "flags",
      header: "Flags",
      enableSorting: false,
      accessorFn: (r) => flags(r).map((flag) => flag.label).join(" "),
      filterFn: (row, _id, value) => {
        const found = flags(row.original)
        return value === "any" ? found.length > 0 : found.some((flag) => flag.label === value)
      },
      cell: ({ row }) => {
        const found = flags(row.original)
        if (found.length === 0) return <span className="text-muted-foreground">—</span>
        return (
          <div className="flex flex-wrap gap-1">
            {found.map((flag) => (
              <StatusPill key={flag.label} status={flag.label} tone={flag.tone} />
            ))}
          </div>
        )
      },
    },
    {
      id: "servers",
      header: "Servers",
      accessorFn: (r) => r.servers,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className="tabular-nums">
          {row.original.servers}
          {row.original.suspended_servers > 0 && (
            <span className="ml-1.5 text-xs text-red-600">
              {row.original.suspended_servers} suspended
            </span>
          )}
        </span>
      ),
    },
    {
      id: "outgoing",
      header: "Outgoing",
      accessorFn: (r) => r[range].outgoing,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className="inline-block min-w-16 tabular-nums">
          {count(row.original[range].outgoing)}
        </span>
      ),
    },
    {
      id: "bounces",
      header: "Bounces",
      accessorFn: (r) => (r[range].total > 0 ? r[range].bounced / r[range].total : -1),
      meta: { align: "right" },
      cell: ({ row }) => {
        const counters = row.original[range]
        const rate = counters.total > 0 ? counters.bounced / counters.total : 0
        return (
          <span
            className={
              rate > 0.1
                ? "inline-block min-w-16 tabular-nums text-red-600"
                : "inline-block min-w-16 tabular-nums"
            }
          >
            {bounceRate(counters)}
          </span>
        )
      },
    },
    {
      id: "held",
      header: "Held",
      accessorFn: (r) => r[range].held,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className="inline-block min-w-16 tabular-nums">
          {count(row.original[range].held)}
        </span>
      ),
    },
    {
      id: "last_send",
      header: "Last message",
      accessorFn: (r) => r.last_message_at ?? "",
      cell: ({ row }) => (
        <span className="text-muted-foreground">{relativeTime(row.original.last_message_at)}</span>
      ),
    },
    {
      id: "two_factor",
      header: "2FA",
      enableSorting: false,
      accessorFn: (r) => r.require_two_factor,
      filterFn: (row, _id, value) =>
        value === "enforced"
          ? row.original.require_two_factor
          : !row.original.require_two_factor,
      cell: ({ row }) =>
        row.original.require_two_factor ? (
          <Badge variant="secondary">Enforced</Badge>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div onClick={(event) => event.stopPropagation()}>
          <Button variant="ghost" size="icon" onClick={() => setDeleting(row.original)}>
            <Trash2Icon className="size-4" />
            <span className="sr-only">Delete {row.original.name}</span>
          </Button>
        </div>
      ),
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Organizations"
          description={`Every organization on this instance, with its traffic over ${windowDescription(
            range,
          )}.`}
          className="mb-0"
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col gap-4">
        <InstanceStrip
          overview={overview.data?.overview}
          range={range}
          loading={overview.isPending}
        />
        <AttentionStrip rows={rows} />
        <div className="flex min-h-0 flex-1 flex-col">
          <DataTable
            columns={columns}
            data={rows}
            loading={overview.isPending}
            searchKeys={["name", "permalink"]}
            searchPlaceholder="Search organizations…"
            emptyText="No organizations on this instance yet."
            actions={<WindowToggle value={range} onChange={setRange} />}
            filters={[
              {
                columnId: "status",
                label: "Status",
                options: ACTIVITY_STATES.map((state) => ({ label: state, value: state })),
              },
              {
                columnId: "flags",
                label: "Flags",
                options: [
                  { label: "Any flag", value: "any" },
                  ...FLAG_LABELS.map((label) => ({ label, value: label })),
                ],
              },
              {
                columnId: "two_factor",
                label: "2FA",
                options: [
                  { label: "Enforced", value: "enforced" },
                  { label: "Not enforced", value: "off" },
                ],
              },
            ]}
            fillHeight
            initialPageSize={20}
          />
        </div>
      </div>

      <ConfirmDialog
        open={deleting !== null}
        onOpenChange={(open) => !open && setDeleting(null)}
        title={`Delete ${deleting?.name ?? "organization"}?`}
        description={
          deleting
            ? `This removes ${plural(deleting.servers, "server")} with every domain, credential and message, along with ${plural(deleting.members, "membership")}. It cannot be undone.`
            : ""
        }
        confirmWord={deleting?.permalink}
        confirmLabel="Delete organization"
        onConfirm={async () => {
          if (deleting) await remove.mutateAsync(deleting.permalink)
        }}
      />
    </Page>
  )
}
