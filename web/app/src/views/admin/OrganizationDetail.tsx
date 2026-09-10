"use client"

// Instance admin: one organization under the microscope. Traffic per
// window, every server with its own numbers, who has access, and the two
// interventions an operator has when a tenant misbehaves — suspend a
// server, or delete the organization.

import { useState } from "react"
import Link from "next/link"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { useRouter } from "next/navigation"
import { type ColumnDef } from "@tanstack/react-table"
import { ExternalLinkIcon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, formatDate, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { DataTable } from "@/components/ui/data-table"
import { Skeleton } from "@/components/ui/skeleton"
import { StatusPill } from "@/components/status-pill"
import { relativeTime } from "@/lib/api-p1"
import {
  adminApi,
  ApiError,
  type Membership,
  type ServerVolume,
  type VolumeCounters,
} from "@/lib/api"
import {
  activity,
  bounceRate,
  count,
  flags,
  plural,
  WINDOWS,
  windowLabel,
} from "@/views/admin/volume"

const COUNTERS: { key: keyof VolumeCounters; label: string }[] = [
  { key: "outgoing", label: "Outgoing" },
  { key: "incoming", label: "Inbound" },
  { key: "sent", label: "Delivered" },
  { key: "held", label: "Held" },
  { key: "failed", label: "Failed" },
  { key: "bounced", label: "Bounced" },
]

/// The three windows as rows, the counters as columns. A fixed matrix of
/// one tenant's numbers rather than a list of records, so it renders as a
/// plain table styled like the shared one.
function TrafficMatrix({
  windows,
  loading,
}: {
  windows: Record<"day" | "week" | "month", VolumeCounters> | undefined
  loading: boolean
}) {
  return (
    <div className="overflow-x-auto rounded-lg border">
      <table className="w-full text-sm">
        <thead>
          <tr className="bg-muted/50">
            <th className="px-4 py-2 text-left text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
              Window
            </th>
            {COUNTERS.map((counter) => (
              <th
                key={counter.key}
                className="px-4 py-2 text-left text-[10px] font-semibold tracking-wider text-muted-foreground uppercase"
              >
                {counter.label}
              </th>
            ))}
            <th className="px-4 py-2 text-left text-[10px] font-semibold tracking-wider text-muted-foreground uppercase">
              Bounce rate
            </th>
          </tr>
        </thead>
        <tbody>
          {WINDOWS.map((w) => {
            const counters = windows?.[w.value]
            return (
              <tr key={w.value} className="border-t">
                <td className="px-4 py-3 font-medium">{w.label}</td>
                {COUNTERS.map((counter) => (
                  <td key={counter.key} className="px-4 py-3 tabular-nums">
                    {loading || !counters ? (
                      <Skeleton className="h-4 w-10" />
                    ) : (
                      count(counters[counter.key])
                    )}
                  </td>
                ))}
                <td className="px-4 py-3 tabular-nums">
                  {loading || !counters ? (
                    <Skeleton className="h-4 w-10" />
                  ) : (
                    bounceRate(counters)
                  )}
                </td>
              </tr>
            )
          })}
        </tbody>
      </table>
    </div>
  )
}

export default function OrganizationDetail({ org }: { org: string }) {
  const router = useRouter()
  const queryClient = useQueryClient()
  const [deleteOpen, setDeleteOpen] = useState(false)

  const overview = useQuery({
    queryKey: ["admin", "org-overview", org],
    queryFn: () => adminApi.organizations.overview(org),
    retry: false,
  })
  const members = useQuery({
    queryKey: ["members", org],
    queryFn: () => adminApi.members(org).list(),
    retry: false,
  })

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["admin", "org-overview", org] })

  const suspension = useMutation({
    mutationFn: ({ server, suspend }: { server: string; suspend: boolean }) =>
      suspend
        ? adminApi.servers(org).suspend(server)
        : adminApi.servers(org).unsuspend(server),
    onSuccess: (_data, { suspend }) => {
      toast.success(suspend ? "Sending suspended." : "Sending resumed.")
      invalidate()
    },
    onError: (err) =>
      toast.error(err instanceof ApiError ? err.message : "Could not change the server"),
  })
  const remove = useMutation({
    mutationFn: () => adminApi.organizations.delete(org),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["admin", "overview"] })
      router.push("/admin/organizations")
    },
    onError: (err) =>
      toast.error(
        err instanceof ApiError ? err.message : "Could not delete the organization",
      ),
  })

  const organization = overview.data?.overview.organization
  const servers = overview.data?.overview.servers ?? []
  const found = organization ? flags(organization) : []
  const missing = overview.error instanceof ApiError && overview.error.code === "NotFound"

  const serverColumns: ColumnDef<ServerVolume>[] = [
    {
      id: "server",
      header: "Server",
      accessorFn: (r) => r.name,
      cell: ({ row }) => (
        <Link
          href={`/orgs/${org}/servers/${row.original.permalink}`}
          className="font-medium transition-colors group-hover:text-primary"
        >
          <span className="block truncate">{row.original.name}</span>
          <span className="block truncate font-mono text-xs font-normal text-muted-foreground">
            {row.original.permalink}
          </span>
        </Link>
      ),
    },
    {
      id: "mode",
      header: "Mode",
      accessorFn: (r) => r.mode,
      cell: ({ row }) => (
        <Badge variant="secondary">
          {row.original.mode === "Development" ? "Sandbox" : "Live"}
        </Badge>
      ),
    },
    {
      id: "state",
      header: "State",
      enableSorting: false,
      accessorFn: (r) => (r.suspended ? "Suspended" : "Active"),
      cell: ({ row }) =>
        row.original.suspended ? (
          <StatusPill status="Suspended" tone="red" />
        ) : (
          <StatusPill status="Active" />
        ),
    },
    ...WINDOWS.map(
      (w): ColumnDef<ServerVolume> => ({
        id: `outgoing_${w.value}`,
        header: `Outgoing ${w.label}`,
        accessorFn: (r) => r[w.value].outgoing,
        meta: { align: "right" },
        cell: ({ row }) => (
          <span className="tabular-nums">{count(row.original[w.value].outgoing)}</span>
        ),
      }),
    ),
    {
      id: "bounces",
      header: "Bounces 30d",
      accessorFn: (r) => (r.month.total > 0 ? r.month.bounced / r.month.total : -1),
      meta: { align: "right" },
      cell: ({ row }) => <span className="tabular-nums">{bounceRate(row.original.month)}</span>,
    },
    {
      id: "held",
      header: "Held 30d",
      accessorFn: (r) => r.month.held,
      meta: { align: "right" },
      cell: ({ row }) => <span className="tabular-nums">{count(row.original.month.held)}</span>,
    },
    {
      id: "send_limit",
      header: "Send limit",
      accessorFn: (r) => r.send_limit ?? -1,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className="tabular-nums text-muted-foreground">
          {row.original.send_limit === null
            ? "Unlimited"
            : row.original.send_limit.toLocaleString("en-US")}
        </span>
      ),
    },
    {
      id: "last_send",
      header: "Last message",
      accessorFn: (r) => r.last_message_at ?? "",
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {relativeTime(row.original.last_message_at)}
        </span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div onClick={(event) => event.stopPropagation()}>
          <Button
            variant="outline"
            size="sm"
            disabled={suspension.isPending}
            onClick={() =>
              suspension.mutate({
                server: row.original.permalink,
                suspend: !row.original.suspended,
              })
            }
          >
            {row.original.suspended ? "Unsuspend" : "Suspend"}
          </Button>
        </div>
      ),
    },
  ]

  const memberColumns: ColumnDef<Membership>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (r) => `${r.user.first_name} ${r.user.last_name}`.trim(),
      cell: ({ row }) => (
        <span className="block truncate font-medium">
          {`${row.original.user.first_name} ${row.original.user.last_name}`.trim() || "—"}
        </span>
      ),
    },
    {
      id: "email",
      header: "Email",
      accessorFn: (r) => r.user.email_address,
      cell: ({ row }) => (
        <span className="text-muted-foreground">{row.original.user.email_address}</span>
      ),
    },
    {
      id: "role",
      header: "Role",
      accessorFn: (r) => r.role,
      cell: ({ row }) => (
        <Badge variant="secondary" className="capitalize">
          {row.original.role}
        </Badge>
      ),
    },
    {
      id: "since",
      header: "Member since",
      accessorFn: (r) => r.created_at,
      cell: ({ row }) => (
        <span className="text-muted-foreground">{formatDate(row.original.created_at)}</span>
      ),
    },
  ]

  if (missing) {
    return (
      <Page
        header={
          <PageHeader
            title={org}
            backHref="/admin/organizations"
            backLabel="Organizations"
            description="This organization does not exist on this instance."
            className="mb-0"
          />
        }
      >
        <Card>
          <CardContent className="py-8 text-center text-sm text-muted-foreground">
            It may have been deleted. The list shows what is still here.
          </CardContent>
        </Card>
      </Page>
    )
  }

  return (
    <Page
      header={
        <PageHeader
          title={organization?.name ?? org}
          backHref="/admin/organizations"
          backLabel="Organizations"
          description={
            <span className="flex flex-wrap items-center gap-2">
              <span className="font-mono text-xs">{org}</span>
              {organization && (
                <>
                  <StatusPill
                    status={activity(organization).label}
                    tone={activity(organization).tone}
                  />
                  {organization.require_two_factor && (
                    <Badge variant="secondary">2FA enforced</Badge>
                  )}
                  {found.map((flag) => (
                    <StatusPill key={flag.label} status={flag.label} tone={flag.tone} />
                  ))}
                </>
              )}
            </span>
          }
          action={
            <>
              <Button variant="outline" size="sm" asChild>
                <Link href={`/orgs/${org}`}>
                  Open organization <ExternalLinkIcon className="size-4" />
                </Link>
              </Button>
              <Button variant="destructive" size="sm" onClick={() => setDeleteOpen(true)}>
                Delete
              </Button>
            </>
          }
        />
      }
    >
      <div className="space-y-6">
        <section className="space-y-2">
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <h2 className="text-sm font-semibold">Traffic</h2>
            <p className="text-xs text-muted-foreground">
              {organization?.last_message_at
                ? `Last message ${relativeTime(organization.last_message_at)}. Oldest inside the 30-day window: ${relativeTime(
                    organization.first_message_at,
                  )}.`
                : "No messages in the last 30 days."}
            </p>
          </div>
          <TrafficMatrix windows={organization} loading={overview.isPending} />
        </section>

        <section className="space-y-2">
          <div className="flex flex-wrap items-baseline justify-between gap-2">
            <h2 className="text-sm font-semibold">Servers</h2>
            <p className="text-xs text-muted-foreground">
              Outgoing counts cover {WINDOWS.map((w) => windowLabel(w.value)).join(", ")}.
            </p>
          </div>
          <DataTable
            columns={serverColumns}
            data={servers}
            loading={overview.isPending}
            searchKeys={["name", "permalink"]}
            searchPlaceholder="Search servers…"
            emptyText="This organization has no servers."
            filters={[
              {
                columnId: "state",
                label: "State",
                options: [
                  { label: "Active", value: "Active" },
                  { label: "Suspended", value: "Suspended" },
                ],
              },
            ]}
            initialPageSize={10}
          />
        </section>

        <section className="space-y-2">
          <h2 className="text-sm font-semibold">People</h2>
          <DataTable
            columns={memberColumns}
            data={members.data?.members ?? []}
            loading={members.isPending}
            searchKeys={["role"]}
            searchPlaceholder="Search members…"
            emptyText="Nobody has access to this organization."
            initialPageSize={10}
          />
        </section>

        <section className="space-y-2">
          <h2 className="text-sm font-semibold">Danger zone</h2>
          <Card className="border-destructive/40">
            <CardHeader>
              <CardTitle className="text-base">Delete this organization</CardTitle>
              <CardDescription>
                Removes every server with its domains, credentials, messages and memberships.
                Use it on a tenant that is abusing the service, once you have what you need
                from the logs.
              </CardDescription>
            </CardHeader>
            <CardContent>
              <Button variant="destructive" onClick={() => setDeleteOpen(true)}>
                Delete organization
              </Button>
            </CardContent>
          </Card>
        </section>
      </div>

      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        title={`Delete ${organization?.name ?? org}?`}
        description={`This removes ${plural(servers.length, "server")} with every domain, credential and message. It cannot be undone.`}
        confirmWord={org}
        confirmLabel="Delete organization"
        onConfirm={async () => {
          await remove.mutateAsync()
        }}
      />
    </Page>
  )
}
