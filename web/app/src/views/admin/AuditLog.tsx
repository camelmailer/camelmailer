"use client"

// Instance admin: the authentication audit trail.

import { useQuery } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { formatDate, PageHeader } from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import { DataTable } from "@/components/ui/data-table"
import { adminApi, type AuthEvent } from "@/lib/api"

function eventVariant(event: string): "default" | "secondary" | "destructive" | "outline" {
  if (event.includes("failure") || event.includes("locked")) return "destructive"
  if (event.startsWith("login") || event.startsWith("sso")) return "default"
  return "secondary"
}

function eventCategory(event: string): "failure" | "auth" | "other" {
  const variant = eventVariant(event)
  if (variant === "destructive") return "failure"
  if (variant === "default") return "auth"
  return "other"
}

export default function AuditLog() {
  const events = useQuery({
    queryKey: ["admin", "auth-events"],
    queryFn: () => adminApi.authEvents.list(500),
    refetchInterval: 30_000,
  })

  const columns: ColumnDef<AuthEvent>[] = [
    {
      id: "time",
      header: "Time",
      accessorFn: (r) => r.created_at,
      cell: ({ row }) => (
        <span className="block truncate font-medium whitespace-nowrap">
          {formatDate(row.original.created_at)}
        </span>
      ),
    },
    {
      id: "event",
      header: "Event",
      accessorFn: (r) => r.event,
      filterFn: (row, _id, value) => eventCategory(row.original.event) === value,
      cell: ({ row }) => (
        <Badge variant={eventVariant(row.original.event)}>{row.original.event}</Badge>
      ),
    },
    {
      id: "account",
      header: "Account",
      accessorFn: (r) => r.email_address ?? "",
      cell: ({ row }) => <span>{row.original.email_address ?? "—"}</span>,
    },
    {
      id: "ip",
      header: "IP",
      accessorFn: (r) => r.ip_address ?? "",
      cell: ({ row }) => (
        <span className="text-muted-foreground">{row.original.ip_address ?? "—"}</span>
      ),
    },
    {
      id: "user_agent",
      header: "User agent",
      enableSorting: false,
      accessorFn: (r) => r.user_agent ?? "",
      cell: ({ row }) => (
        <span className="block max-w-64 truncate text-muted-foreground">
          {row.original.user_agent ?? "—"}
        </span>
      ),
    },
  ]

  return (
    <div>
      <PageHeader
        title="Audit log"
        description="Logins, failures, lockouts, password / role / SSO events."
      />
      <DataTable
        columns={columns}
        data={events.data?.auth_events ?? []}
        loading={events.isPending}
        searchKeys={["event", "email_address", "ip_address", "user_agent"]}
        searchPlaceholder="Search events…"
        emptyText="No authentication events recorded yet."
        filters={[
          {
            columnId: "event",
            label: "Type",
            options: [
              { label: "Failures & lockouts", value: "failure" },
              { label: "Logins & SSO", value: "auth" },
              { label: "Other", value: "other" },
            ],
          },
        ]}
        initialPageSize={20}
      />
    </div>
  )
}
