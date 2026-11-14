"use client"

// Instance admin: the cluster topology. Which processes run on which host,
// in which role and version, when each was last heard from, and which
// worker holds which queue leases; the addresses the installation receives
// and sends on. Reads GET /api/v2/admin/cluster.

import { useQuery } from "@tanstack/react-query"
import { TriangleAlertIcon } from "lucide-react"
import { EmptyState, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { StatusPill } from "@/components/status-pill"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { DataTable, type DataTableColumn } from "@/components/ui/data-table"
import { relativeTime } from "@/lib/api-p1"
import {
  hostRows,
  infrastructureApi,
  shortDuration,
  type ClusterNode,
  type HostRow,
} from "@/lib/infrastructure"
import { InfrastructureNav } from "./InfrastructureNav"

const STATUS_TONE = { live: "green", stale: "amber", stopped: "gray" } as const

function NodeStatusPill({ status }: { status: ClusterNode["status"] }) {
  return <StatusPill status={status} tone={STATUS_TONE[status]} />
}

function Fact({ label, value }: { label: string; value: string | null | undefined }) {
  return (
    <div className="min-w-0">
      <div className="text-xs font-medium text-muted-foreground uppercase tracking-wider">{label}</div>
      <div className="mt-1 truncate font-mono text-sm">{value || "—"}</div>
    </div>
  )
}

const hostColumns: DataTableColumn<HostRow>[] = [
  {
    id: "node_name",
    header: "Host",
    accessorFn: (r) => r.node_name,
    cell: ({ row }) => <span className="block truncate font-semibold">{row.original.node_name}</span>,
  },
  {
    id: "roles",
    header: "Roles",
    accessorFn: (r) => r.roles.join(", "),
    cell: ({ row }) => (
      <div className="flex flex-wrap gap-1">
        {row.original.roles.length
          ? row.original.roles.map((role) => (
              <span key={role} className="rounded bg-muted px-1.5 py-0.5 text-xs font-medium">
                {role}
              </span>
            ))
          : "—"}
      </div>
    ),
  },
  {
    id: "version",
    header: "Version",
    accessorFn: (r) => r.versions.join(", "),
    cell: ({ row }) => <span className="font-mono text-xs">{row.original.versions.join(", ") || "—"}</span>,
  },
  {
    id: "public_addresses",
    header: "Public addresses",
    accessorFn: (r) => r.public_addresses.join(", "),
    cell: ({ row }) => (
      <span className="font-mono text-xs text-muted-foreground">
        {row.original.public_addresses.join(", ") || "—"}
      </span>
    ),
  },
  {
    id: "leases",
    header: "Leases",
    accessorFn: (r) => r.leases,
    meta: { align: "right" },
    cell: ({ row }) => <span className="inline-block min-w-12 tabular-nums">{row.original.leases}</span>,
  },
  {
    id: "last_seen_at",
    header: "Last heartbeat",
    accessorFn: (r) => r.last_seen_at,
    cell: ({ row }) => <span className="text-muted-foreground">{relativeTime(row.original.last_seen_at)}</span>,
  },
  {
    id: "status",
    header: "Status",
    accessorFn: (r) => r.status,
    cell: ({ row }) => <NodeStatusPill status={row.original.status} />,
  },
]

const processColumns: DataTableColumn<ClusterNode>[] = [
  {
    id: "instance_id",
    header: "Process",
    accessorFn: (r) => r.instance_id,
    cell: ({ row }) => (
      <div className="min-w-0">
        <div className="truncate font-mono text-xs font-medium">{row.original.instance_id}</div>
        <div className="text-xs text-muted-foreground">{row.original.node_name}</div>
      </div>
    ),
  },
  { id: "role", header: "Role", accessorFn: (r) => r.role },
  {
    id: "version",
    header: "Version",
    accessorFn: (r) => r.version,
    cell: ({ row }) => <span className="font-mono text-xs">{row.original.version}</span>,
  },
  {
    id: "queued",
    header: "Messages",
    accessorFn: (r) => r.leases?.queued_messages ?? -1,
    meta: { align: "right" },
    cell: ({ row }) => (
      <span className="inline-block min-w-12 tabular-nums">{row.original.leases?.queued_messages ?? "—"}</span>
    ),
  },
  {
    id: "webhooks",
    header: "Webhooks",
    accessorFn: (r) => r.leases?.webhook_requests ?? -1,
    meta: { align: "right" },
    cell: ({ row }) => (
      <span className="inline-block min-w-12 tabular-nums">{row.original.leases?.webhook_requests ?? "—"}</span>
    ),
  },
  {
    id: "oldest",
    header: "Oldest lock",
    accessorFn: (r) => r.leases?.oldest_lock_age_seconds ?? -1,
    meta: { align: "right" },
    cell: ({ row }) => (
      <span className="inline-block min-w-12 tabular-nums">
        {shortDuration(row.original.leases?.oldest_lock_age_seconds)}
      </span>
    ),
  },
  {
    id: "last_seen_at",
    header: "Last heartbeat",
    accessorFn: (r) => r.last_seen_at,
    cell: ({ row }) => <span className="text-muted-foreground">{relativeTime(row.original.last_seen_at)}</span>,
  },
  {
    id: "status",
    header: "Status",
    accessorFn: (r) => r.status,
    cell: ({ row }) => <NodeStatusPill status={row.original.status} />,
  },
]

export default function Topology() {
  const cluster = useQuery({
    queryKey: ["admin", "cluster"],
    queryFn: infrastructureApi.cluster,
    refetchInterval: 30_000,
  })
  const data = cluster.data
  const hosts = data ? hostRows(data.nodes) : []

  return (
    <Page
      header={
        <>
          <PageHeader
            title="Infrastructure"
            description="The hosts and processes of this installation, read from their heartbeats."
          />
          <InfrastructureNav />
        </>
      }
    >
      <div className="flex flex-col gap-6">
        {cluster.isError && (
          <Alert variant="destructive">
            <AlertTitle>The topology could not be loaded</AlertTitle>
            <AlertDescription>{String(cluster.error)}</AlertDescription>
          </Alert>
        )}
        {data?.mixed_versions && (
          <Alert>
            <TriangleAlertIcon />
            <AlertTitle>Live processes run different versions</AlertTitle>
            <AlertDescription>
              {data.versions.join(", ")}. That is expected during a rolling update and should end
              when it does.
            </AlertDescription>
          </Alert>
        )}

        <Card>
          <CardHeader>
            <CardTitle>Entry points</CardTitle>
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-3">
            <Fact label="Inbound host name" value={data?.cluster.inbound_hostname} />
            <Fact label="Load balancer" value={data?.cluster.load_balancer_address} />
            <Fact
              label="Stale after"
              value={data ? `${data.cluster.node_stale_seconds} s without a heartbeat` : null}
            />
          </CardContent>
        </Card>

        <section className="flex flex-col gap-3">
          <h2 className="text-sm font-semibold">Hosts</h2>
          <DataTable
            columns={hostColumns}
            data={hosts}
            loading={cluster.isLoading}
            searchKeys={["node_name"]}
            filters={[
              {
                columnId: "status",
                label: "Status",
                options: [
                  { label: "Live", value: "live" },
                  { label: "Stale", value: "stale" },
                  { label: "Stopped", value: "stopped" },
                ],
              },
            ]}
            emptyText="No process has registered yet. Every web, SMTP and worker process registers when it starts."
          />
        </section>

        <section className="flex flex-col gap-3">
          <h2 className="text-sm font-semibold">Processes and leases</h2>
          <DataTable
            columns={processColumns}
            data={data?.nodes ?? []}
            loading={cluster.isLoading}
            searchKeys={["instance_id", "node_name"]}
            filters={[
              {
                columnId: "role",
                label: "Role",
                options: [
                  { label: "SMTP", value: "smtp" },
                  { label: "Web", value: "web" },
                  { label: "Worker", value: "worker" },
                ],
              },
            ]}
            emptyText="No process has registered yet."
          />
          {data && data.unregistered_leases.length > 0 && (
            <p className="text-sm text-muted-foreground">
              {data.unregistered_leases.reduce((sum, l) => sum + l.queued_messages, 0)} queued
              messages are locked by workers that are not registered (
              {data.unregistered_leases.map((l) => l.locked_by).join(", ")}). Another worker takes
              them over once their lease runs out.
            </p>
          )}
        </section>

        <section className="flex flex-col gap-3">
          <h2 className="text-sm font-semibold">IP pools</h2>
          {data && data.ip_pools.length === 0 ? (
            <EmptyState>No IP pools. Servers send from the public addresses of their worker hosts.</EmptyState>
          ) : (
            <div className="grid gap-4 md:grid-cols-2">
              {(data?.ip_pools ?? []).map((pool) => (
                <Card key={pool.id}>
                  <CardHeader>
                    <CardTitle className="flex items-center gap-2">
                      {pool.name}
                      {pool.default && <StatusPill status="default" tone="gray" />}
                    </CardTitle>
                  </CardHeader>
                  <CardContent className="flex flex-col gap-2">
                    {pool.addresses.length === 0 && (
                      <p className="text-sm text-muted-foreground">No addresses.</p>
                    )}
                    {pool.addresses.map((address) => (
                      <div key={address.id} className="flex flex-wrap items-baseline justify-between gap-2 text-sm">
                        <span className="font-mono">
                          {address.ipv4}
                          {address.ipv6 ? ` / ${address.ipv6}` : ""}
                        </span>
                        <span className="font-mono text-xs text-muted-foreground">{address.hostname || "—"}</span>
                      </div>
                    ))}
                  </CardContent>
                </Card>
              ))}
            </div>
          )}
        </section>
      </div>
    </Page>
  )
}
