"use client"

// Campaigns: a server-level, first-class broadcast entity with planning.
// Each campaign targets one broadcast stream's subscribers and moves through
// draft → scheduled → sending → sent (or failed / canceled). This is its own
// sidebar section (like Recipients), not nested under a stream. Talks to the
// per-server messaging API (`X-Server-API-Key`) via useServerMessagingApi.

import { useMemo, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { KeyRoundIcon, MegaphoneIcon, PlusIcon, SendIcon } from "lucide-react"
import { toast } from "sonner"
import { formatDate, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { EmptyState } from "@/components/empty-state"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
import { DataTable } from "@/components/ui/data-table"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Textarea } from "@/components/ui/textarea"
import { cn } from "@/lib/utils"
import { ApiError, type Campaign, type CampaignWithStream } from "@/lib/api"
import { useServerMessagingApi } from "@/lib/api-p2"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

// A campaign is settled once it has fully sent, failed, or been canceled;
// draft and scheduled are editable, sending is in-flight.
const STATUS_VARIANT: Record<Campaign["status"], "default" | "secondary" | "outline" | "destructive"> = {
  draft: "outline",
  scheduled: "outline",
  sending: "default",
  sent: "secondary",
  failed: "destructive",
  canceled: "outline",
}

function StatusBadge({ status }: { status: Campaign["status"] }) {
  return (
    <Badge
      variant={STATUS_VARIANT[status]}
      className={cn(status === "canceled" && "text-muted-foreground")}
    >
      {status}
    </Badge>
  )
}

// A credential-gated empty state shared by every campaign screen — campaigns
// speak to the server's own API, so no API credential means nothing to show.
function ConnectCredential({ org, server }: { org: string; server: string }) {
  return (
    <EmptyState
      icon={KeyRoundIcon}
      title="Connect an API credential"
      description="Campaigns talk to the server's own API. Create an API credential first, then come back here."
      action={{
        label: "Create API credential",
        href: `/orgs/${org}/servers/${server}/credentials`,
      }}
    />
  )
}

// ---------------------------------------------------------------- list

/// The standalone Campaigns view (its own sidebar item): every campaign on
/// this server with its audience, status, schedule and progress. Rows open the
/// campaign detail; "New campaign" opens the create form.
export function CampaignsList({ org, server }: { org: string; server: string }) {
  const { api, isLoading } = useServerMessagingApi(org, server)
  const base = `/orgs/${org}/servers/${server}/campaigns`

  const campaignsQuery = useQuery({
    queryKey: ["sapi-campaigns", org, server],
    queryFn: () => api!.campaigns.list(),
    enabled: api !== null,
    // Keep the list live while any campaign is still expanding or waiting.
    refetchInterval: (query) =>
      query.state.data?.campaigns.some(
        (c) => c.status === "sending" || c.status === "scheduled",
      )
        ? 5000
        : false,
  })
  const rows = campaignsQuery.data?.campaigns ?? []

  const columns: ColumnDef<CampaignWithStream>[] = [
    {
      id: "subject",
      header: "Subject",
      accessorFn: (c) => c.subject,
      cell: ({ row }) => (
        <Link
          href={`${base}/${row.original.id}`}
          className="block max-w-80 truncate font-medium transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.subject || `Campaign #${row.original.id}`}
        </Link>
      ),
    },
    {
      id: "audience",
      header: "Audience",
      accessorFn: (c) => c.stream?.name ?? "",
      cell: ({ row }) =>
        row.original.stream ? (
          <span className="text-muted-foreground">{row.original.stream.name}</span>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
    {
      id: "status",
      header: "Status",
      accessorFn: (c) => c.status,
      cell: ({ row }) => <StatusBadge status={row.original.status} />,
    },
    {
      id: "schedule",
      header: "Schedule",
      accessorFn: (c) => c.scheduled_at ?? "",
      cell: ({ row }) =>
        row.original.scheduled_at ? (
          <span className="whitespace-nowrap text-muted-foreground">
            {formatDate(row.original.scheduled_at)}
          </span>
        ) : (
          <span className="text-muted-foreground">—</span>
        ),
    },
    {
      id: "progress",
      header: "Progress",
      accessorFn: (c) => c.sent,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className="tabular-nums text-muted-foreground">
          {row.original.sent}/{row.original.total}
        </span>
      ),
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Campaigns"
          description="One-off broadcasts to a stream's subscribers — draft, schedule for later, or send now."
          className="mb-0"
          action={
            api ? (
              <Button size="sm" asChild>
                <Link href={`${base}/new`}>
                  <PlusIcon className="size-4" /> New campaign
                </Link>
              </Button>
            ) : undefined
          }
        />
      }
    >
      {!api && !isLoading ? (
        <ConnectCredential org={org} server={server} />
      ) : (
        <div className="flex min-h-0 flex-1 flex-col">
          <DataTable
            columns={columns}
            data={rows}
            loading={campaignsQuery.isLoading || isLoading}
            fillHeight
            searchKeys={["subject"]}
            searchPlaceholder="Search campaigns…"
            emptyText="No campaigns yet. Create one to broadcast to your subscribers."
            initialPageSize={20}
          />
        </div>
      )}
    </Page>
  )
}

// ---------------------------------------------------------------- form

type SendMode = "now" | "schedule" | "draft"

/// A `datetime-local` input value (local wall-clock, no timezone) from an ISO
/// string, and back. The form stores the local string; submission converts it
/// to an RFC3339/UTC instant via `new Date(local).toISOString()`.
function isoToLocalInput(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ""
  const pad = (n: number) => String(n).padStart(2, "0")
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(
    d.getHours(),
  )}:${pad(d.getMinutes())}`
}

/// Create (no id) or edit (id) a campaign. Fields: audience (a broadcast
/// stream), name, from, subject, HTML body, and a "When to send" control.
export function CampaignForm({
  org,
  server,
  id,
}: {
  org: string
  server: string
  id?: number
}) {
  const router = useRouter()
  const queryClient = useQueryClient()
  const { api, isLoading } = useServerMessagingApi(org, server)
  const base = `/orgs/${org}/servers/${server}/campaigns`
  const editing = id !== undefined

  const streamsQuery = useQuery({
    queryKey: ["sapi-streams", org, server],
    queryFn: () => api!.streams.list(),
    enabled: api !== null,
  })
  const broadcastStreams = (streamsQuery.data?.streams ?? []).filter(
    (s) => s.stream_type === "broadcast" && !s.archived,
  )

  // On edit, prefetch the campaign to seed the form.
  const existing = useQuery({
    queryKey: ["sapi-campaign", org, server, id],
    queryFn: () => api!.campaigns.get(id!),
    enabled: api !== null && editing,
  })

  const [stream, setStream] = useState("")
  const [name, setName] = useState("")
  const [from, setFrom] = useState("")
  const [subject, setSubject] = useState("")
  const [htmlBody, setHtmlBody] = useState("")
  const [mode, setMode] = useState<SendMode>("draft")
  const [scheduleLocal, setScheduleLocal] = useState("")
  // Seed the form once the edited campaign arrives.
  const [seeded, setSeeded] = useState(false)
  if (editing && !seeded && existing.data) {
    const c = existing.data.campaign
    setStream(c.stream?.permalink ?? "")
    setName(c.name ?? "")
    setFrom(c.from)
    setSubject(c.subject)
    setHtmlBody(c.html_body ?? "")
    if (c.scheduled_at) {
      setMode("schedule")
      setScheduleLocal(isoToLocalInput(c.scheduled_at))
    } else {
      setMode("draft")
    }
    setSeeded(true)
  }

  const save = useMutation({
    mutationFn: () => {
      const scheduled =
        mode === "schedule" && scheduleLocal
          ? { scheduled_at: new Date(scheduleLocal).toISOString() }
          : {}
      if (editing) {
        return api!.campaigns.update(id!, {
          ...(name.trim() ? { name: name.trim() } : {}),
          from,
          subject,
          ...(htmlBody ? { html_body: htmlBody } : {}),
          ...scheduled,
        })
      }
      return api!.campaigns.create({
        stream,
        ...(name.trim() ? { name: name.trim() } : {}),
        from,
        subject,
        ...(htmlBody ? { html_body: htmlBody } : {}),
        ...scheduled,
        ...(mode === "now" ? { send_now: true } : {}),
      })
    },
    onSuccess: (data) => {
      queryClient.invalidateQueries({ queryKey: ["sapi-campaigns"] })
      queryClient.invalidateQueries({ queryKey: ["sapi-campaign"] })
      toast.success(editing ? "Campaign saved" : "Campaign created")
      router.push(`${base}/${data.campaign.id}`)
    },
    onError: (err) => errorToast(err, editing ? "Could not save the campaign" : "Could not create the campaign"),
  })

  const canSubmit =
    (editing || stream) &&
    from.includes("@") &&
    subject.trim().length > 0 &&
    (mode !== "schedule" || scheduleLocal.length > 0)

  // The "When to send" options — edit does not offer an immediate send (that
  // lives on the detail page), so create-only "Send now" is filtered out there.
  const sendOptions: { value: SendMode; label: string }[] = [
    ...(editing ? [] : [{ value: "now" as const, label: "Send now" }]),
    { value: "schedule", label: "Schedule" },
    { value: "draft", label: "Save as draft" },
  ]

  return (
    <Page
      header={
        <PageHeader
          className="mb-0"
          backHref={base}
          backLabel="Campaigns"
          title={editing ? "Edit campaign" : "New campaign"}
          description="Compose a broadcast and choose whether to send it now, schedule it, or keep it as a draft."
        />
      }
    >
      {!api && !isLoading ? (
        <ConnectCredential org={org} server={server} />
      ) : editing && existing.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading campaign…</p>
      ) : (
        <div className="grid max-w-2xl gap-4">
          <div className="grid gap-2">
            <Label>Audience</Label>
            <Select value={stream} onValueChange={setStream} disabled={editing}>
              <SelectTrigger>
                <SelectValue placeholder="Choose a broadcast stream" />
              </SelectTrigger>
              <SelectContent>
                {broadcastStreams.map((s) => (
                  <SelectItem key={s.id} value={s.permalink}>
                    {s.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {!editing && broadcastStreams.length === 0 && streamsQuery.isSuccess && (
              <p className="text-xs text-muted-foreground">
                No broadcast streams yet.{" "}
                <Link
                  href={`/orgs/${org}/servers/${server}/streams`}
                  className="font-medium underline underline-offset-2"
                >
                  Create one
                </Link>{" "}
                to broadcast to its subscribers.
              </p>
            )}
          </div>

          <div className="grid gap-2">
            <Label>Name (optional)</Label>
            <Input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Internal name — helps you find this later"
            />
          </div>

          <div className="grid gap-2">
            <Label>From</Label>
            <Input
              value={from}
              onChange={(e) => setFrom(e.target.value)}
              placeholder="news@yourdomain.com"
            />
          </div>

          <div className="grid gap-2">
            <Label>Subject</Label>
            <Input value={subject} onChange={(e) => setSubject(e.target.value)} />
          </div>

          <div className="grid gap-2">
            <Label>HTML body</Label>
            <Textarea
              rows={10}
              value={htmlBody}
              onChange={(e) => setHtmlBody(e.target.value)}
              className="font-mono text-xs"
              placeholder="<p>Hello subscribers…</p>"
            />
          </div>

          <div className="grid gap-2">
            <Label>When to send</Label>
            <div className="flex flex-wrap gap-2">
              {sendOptions.map((option) => (
                <Button
                  key={option.value}
                  type="button"
                  variant={mode === option.value ? "default" : "outline"}
                  size="sm"
                  onClick={() => setMode(option.value)}
                >
                  {option.label}
                </Button>
              ))}
            </div>
            {mode === "schedule" && (
              <Input
                type="datetime-local"
                value={scheduleLocal}
                onChange={(e) => setScheduleLocal(e.target.value)}
                className="mt-1 max-w-xs"
              />
            )}
          </div>

          <div className="flex items-center gap-2">
            <Button
              className="justify-self-start"
              onClick={() => save.mutate()}
              disabled={save.isPending || !canSubmit}
            >
              {save.isPending
                ? "Saving…"
                : editing
                  ? "Save changes"
                  : mode === "now"
                    ? "Send campaign"
                    : mode === "schedule"
                      ? "Schedule campaign"
                      : "Save draft"}
            </Button>
            <Button variant="ghost" asChild>
              <Link href={base}>Cancel</Link>
            </Button>
          </div>
        </div>
      )}
    </Page>
  )
}

// -------------------------------------------------------------- detail

/// One campaign with its analytics tiles and status-dependent actions. Polls
/// while the campaign is scheduled (waiting to fire) or sending (in flight).
export function CampaignDetailPage({
  org,
  server,
  id,
}: {
  org: string
  server: string
  id: number
}) {
  const queryClient = useQueryClient()
  const { api, isLoading } = useServerMessagingApi(org, server)
  const base = `/orgs/${org}/servers/${server}/campaigns`

  const query = useQuery({
    queryKey: ["sapi-campaign", org, server, id],
    queryFn: () => api!.campaigns.get(id),
    enabled: api !== null,
    refetchInterval: (q) => {
      const status = q.state.data?.campaign.status
      return status === "sending" || status === "scheduled" ? 3000 : false
    },
  })
  const c = query.data?.campaign
  const s = query.data?.stats

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ["sapi-campaign", org, server, id] })
    queryClient.invalidateQueries({ queryKey: ["sapi-campaigns"] })
  }

  const send = useMutation({
    mutationFn: () => api!.campaigns.send(id),
    onSuccess: () => {
      invalidate()
      toast.success("Campaign is sending")
    },
    onError: (err) => errorToast(err, "Could not send the campaign"),
  })
  const cancel = useMutation({
    mutationFn: () => api!.campaigns.cancel(id),
    onSuccess: () => {
      invalidate()
      toast.success("Campaign canceled")
    },
    onError: (err) => errorToast(err, "Could not cancel the campaign"),
  })

  const editable = c?.status === "draft" || c?.status === "scheduled"

  const tiles: [string, number | undefined][] = [
    ["Recipients", s?.total],
    ["Sent", s?.sent],
    ["Delivered", s?.delivered],
    ["Failed", s?.failed],
    ["Opened", s?.opened],
    ["Clicked", s?.clicked],
    ["Unsubscribed", s?.unsubscribed],
  ]

  return (
    <Page
      header={
        <PageHeader
          className="mb-0 items-start"
          backHref={base}
          backLabel="Campaigns"
          title={c?.subject || `Campaign #${id}`}
          description={
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
              {c && <StatusBadge status={c.status} />}
              {c?.stream && <span>To {c.stream.name}</span>}
              {c?.from && <span>From {c.from}</span>}
              {c?.scheduled_at ? (
                <span>Scheduled {formatDate(c.scheduled_at)}</span>
              ) : (
                c?.created_at && <span>Created {formatDate(c.created_at)}</span>
              )}
            </span>
          }
          action={
            editable ? (
              <div className="flex flex-wrap items-center gap-2">
                <Button
                  size="sm"
                  onClick={() => send.mutate()}
                  disabled={send.isPending}
                >
                  <SendIcon className="size-4" /> Send now
                </Button>
                <Button variant="outline" size="sm" asChild>
                  <Link href={`${base}/${id}/edit`}>Edit</Link>
                </Button>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => cancel.mutate()}
                  disabled={cancel.isPending}
                >
                  Cancel
                </Button>
              </div>
            ) : undefined
          }
        />
      }
    >
      {!api && !isLoading ? (
        <ConnectCredential org={org} server={server} />
      ) : !c && query.isLoading ? (
        <p className="text-sm text-muted-foreground">Loading campaign…</p>
      ) : !c ? (
        <p className="text-sm text-muted-foreground">This campaign could not be loaded.</p>
      ) : (
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {tiles.map(([label, value]) => (
            <Card key={label}>
              <CardContent className="p-4">
                <p className="text-xs text-muted-foreground">{label}</p>
                <p className="text-2xl font-semibold tabular-nums">
                  {value ?? (query.isLoading ? "—" : 0)}
                </p>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
    </Page>
  )
}
