"use client"

// The dashboard's Servers and Organizations tables — both use the shared
// DataTable (datatable-ux rules). Servers show real 30-day counters from
// GET .../servers/stats plus a derived deliverability status.

import Link from "next/link"
import { useQueries, useQuery } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { Badge } from "@/components/ui/badge"
import { StatusPill, type PillTone } from "@/components/status-pill"
import { DataTable } from "@/components/ui/data-table"
import { adminApi, type Server } from "@/lib/api"
import { serverDotColor } from "@/lib/api-extras"
import { orgInitials } from "@/components/org-switcher"

type OrgItem = {
  organization: { id: number; name: string; permalink: string }
  role: string | null
}

const num = (n: number) => (n === 0 ? "–" : n.toLocaleString("en-US"))

// ── Servers ────────────────────────────────────────────────────────────

type ServerRow = {
  server: Server
  name: string
  orgName: string
  orgPermalink: string
  outgoing: number
  incoming: number
  bounced: number
  total: number
}

function deliverability(row: ServerRow): { label: string; tone: PillTone } {
  if (row.server.mode === "Development") return { label: "Sandbox", tone: "amber" }
  if (row.server.suspended) return { label: "Suspended", tone: "red" }
  if (row.total === 0) return { label: "Never used", tone: "gray" }
  if (row.bounced / row.total > 0.1) return { label: "Check bounces", tone: "red" }
  return { label: "Good", tone: "green" }
}

const DELIVERABILITY_FILTER = {
  columnId: "deliverability",
  label: "Deliverability",
  options: [
    { label: "Good", value: "Good" },
    { label: "Sandbox", value: "Sandbox" },
    { label: "Never used", value: "Never used" },
    { label: "Check bounces", value: "Check bounces" },
    { label: "Suspended", value: "Suspended" },
  ],
}

function serverColumns(showOrg: boolean): ColumnDef<ServerRow>[] {
  return [
  {
    id: "server",
    header: "Server",
    accessorFn: (r) => r.name,
    cell: ({ row }) => {
      const s = row.original.server
      return (
        <Link
          href={`/orgs/${row.original.orgPermalink}/servers/${s.permalink}`}
          className="flex items-center gap-2 font-medium transition-colors group-hover:text-primary"
        >
          <span
            aria-hidden
            className="size-2 shrink-0 rounded-full"
            style={{ backgroundColor: serverDotColor(s) }}
          />
          <span className="truncate">{s.name}</span>
        </Link>
      )
    },
  },
  ...(showOrg
    ? [
        {
          id: "organization",
          header: "Organization",
          accessorFn: (r: ServerRow) => r.orgName,
          cell: ({ row }: { row: { original: ServerRow } }) => (
            <Link
              href={`/orgs/${row.original.orgPermalink}`}
              className="text-muted-foreground hover:text-foreground hover:underline"
            >
              {row.original.orgName}
            </Link>
          ),
        } satisfies ColumnDef<ServerRow>,
      ]
    : []),
  {
    id: "outgoing",
    header: "Outgoing 30d",
    accessorFn: (r) => r.outgoing,
    cell: ({ row }) => num(row.original.outgoing),
    meta: { align: "right" },
  },
  {
    id: "incoming",
    header: "Inbound 30d",
    accessorFn: (r) => r.incoming,
    cell: ({ row }) => num(row.original.incoming),
    meta: { align: "right" },
  },
  {
    id: "deliverability",
    header: "Deliverability",
    enableSorting: false,
    accessorFn: (r) => deliverability(r).label,
    filterFn: (row, _id, value) => deliverability(row.original).label === value,
    cell: ({ row }) => {
      const d = deliverability(row.original)
      return <StatusPill status={d.label} tone={d.tone} />
    },
  },
  ]
}

export function ServersTable({ orgs }: { orgs: OrgItem[] }) {
  const serverQ = useQueries({
    queries: orgs.map((o) => ({
      queryKey: ["servers", o.organization.permalink],
      queryFn: () => adminApi.servers(o.organization.permalink).list(),
    })),
  })
  const statsQ = useQueries({
    queries: orgs.map((o) => ({
      queryKey: ["server-stats", o.organization.permalink],
      queryFn: () => adminApi.servers(o.organization.permalink).stats(),
    })),
  })

  const rows: ServerRow[] = []
  orgs.forEach((o, i) => {
    const servers = serverQ[i]?.data?.servers ?? []
    const byPermalink = new Map((statsQ[i]?.data?.stats ?? []).map((s) => [s.server, s]))
    for (const server of servers) {
      const st = byPermalink.get(server.permalink)
      rows.push({
        server,
        name: server.name,
        orgName: o.organization.name,
        orgPermalink: o.organization.permalink,
        outgoing: st?.outgoing ?? 0,
        incoming: st?.incoming ?? 0,
        bounced: st?.bounced ?? 0,
        total: st?.total ?? 0,
      })
    }
  })

  return (
    <DataTable
      columns={serverColumns(true)}
      data={rows}
      loading={serverQ.some((q) => q.isPending)}
      searchKeys={["name", "orgName"]}
      searchPlaceholder="Search servers…"
      emptyText="No servers yet."
      filters={[DELIVERABILITY_FILTER]}
    />
  )
}

// Org-scoped servers table (no Organization column) — used on the
// org-level Servers page.
export function OrgServersTable({ org, fillHeight }: { org: string; fillHeight?: boolean }) {
  const serversQ = useQuery({
    queryKey: ["servers", org],
    queryFn: () => adminApi.servers(org).list(),
  })
  const statsQ = useQuery({
    queryKey: ["server-stats", org],
    queryFn: () => adminApi.servers(org).stats(),
  })

  const byPermalink = new Map((statsQ.data?.stats ?? []).map((s) => [s.server, s]))
  const rows: ServerRow[] = (serversQ.data?.servers ?? []).map((server) => {
    const st = byPermalink.get(server.permalink)
    return {
      server,
      name: server.name,
      orgName: "",
      orgPermalink: org,
      outgoing: st?.outgoing ?? 0,
      incoming: st?.incoming ?? 0,
      bounced: st?.bounced ?? 0,
      total: st?.total ?? 0,
    }
  })

  return (
    <DataTable
      columns={serverColumns(false)}
      data={rows}
      loading={serversQ.isPending}
      searchKeys={["name"]}
      searchPlaceholder="Search servers…"
      emptyText="No servers yet."
      filters={[DELIVERABILITY_FILTER]}
      fillHeight={fillHeight}
    />
  )
}

// ── Organizations ──────────────────────────────────────────────────────

type OrgRow = {
  name: string
  permalink: string
  role: string | null
  serverCount: number | null
}

const ORG_COLUMNS: ColumnDef<OrgRow>[] = [
  {
    id: "organization",
    header: "Organization",
    accessorFn: (r) => r.name,
    cell: ({ row }) => (
      <Link
        href={`/orgs/${row.original.permalink}`}
        className="flex items-center gap-2.5 font-medium transition-colors group-hover:text-primary"
      >
        <span className="flex size-6 shrink-0 items-center justify-center rounded-md bg-primary text-[10px] font-semibold text-primary-foreground">
          {orgInitials(row.original.name)}
        </span>
        <span className="truncate">{row.original.name}</span>
      </Link>
    ),
  },
  {
    id: "role",
    header: "Role",
    accessorFn: (r) => r.role ?? "",
    cell: ({ row }) =>
      row.original.role ? (
        <Badge variant="secondary" className="capitalize">
          {row.original.role}
        </Badge>
      ) : (
        <span className="text-muted-foreground">–</span>
      ),
  },
  {
    id: "servers",
    header: "Servers",
    accessorFn: (r) => r.serverCount ?? -1,
    cell: ({ row }) =>
      row.original.serverCount === null ? "…" : num(row.original.serverCount),
    meta: { align: "right" },
  },
]

export function OrganizationsTable({ orgs }: { orgs: OrgItem[] }) {
  const serverQ = useQueries({
    queries: orgs.map((o) => ({
      queryKey: ["servers", o.organization.permalink],
      queryFn: () => adminApi.servers(o.organization.permalink).list(),
    })),
  })

  const rows: OrgRow[] = orgs.map((o, i) => ({
    name: o.organization.name,
    permalink: o.organization.permalink,
    role: o.role,
    serverCount: serverQ[i]?.isSuccess ? (serverQ[i].data?.servers.length ?? 0) : null,
  }))

  return (
    <DataTable
      columns={ORG_COLUMNS}
      data={rows}
      searchKeys={["name"]}
      searchPlaceholder="Search organizations…"
      emptyText="No organizations yet."
    />
  )
}
