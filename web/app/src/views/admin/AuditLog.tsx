"use client"

// Instance admin: the authentication audit trail.

import { useQuery } from "@tanstack/react-query"
import { formatDate, PageHeader } from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { adminApi } from "@/lib/api"

function eventVariant(event: string): "default" | "secondary" | "destructive" | "outline" {
  if (event.includes("failure") || event.includes("locked")) return "destructive"
  if (event.startsWith("login") || event.startsWith("sso")) return "default"
  return "secondary"
}

export default function AuditLog() {
  const events = useQuery({
    queryKey: ["admin", "auth-events"],
    queryFn: () => adminApi.authEvents.list(500),
    refetchInterval: 30_000,
  })

  return (
    <div>
      <PageHeader
        title="Audit log"
        description="Logins, failures, lockouts, password / role / SSO events."
      />
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Time</TableHead>
            <TableHead>Event</TableHead>
            <TableHead>Account</TableHead>
            <TableHead>IP</TableHead>
            <TableHead>User agent</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {events.data?.auth_events.map((event) => (
            <TableRow key={event.id}>
              <TableCell className="whitespace-nowrap text-muted-foreground">
                {formatDate(event.created_at)}
              </TableCell>
              <TableCell>
                <Badge variant={eventVariant(event.event)}>{event.event}</Badge>
              </TableCell>
              <TableCell>{event.email_address ?? "—"}</TableCell>
              <TableCell className="text-muted-foreground">{event.ip_address ?? "—"}</TableCell>
              <TableCell className="max-w-64 truncate text-muted-foreground">
                {event.user_agent ?? "—"}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </div>
  )
}
