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
import { Cell, Pie, PieChart } from "recharts"
import {
  ChevronRightIcon,
  KeyRoundIcon,
  PlusIcon,
  SendIcon,
} from "lucide-react"
import { toast } from "sonner"
import { formatDate, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { EmptyState } from "@/components/empty-state"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
import { ChartContainer, type ChartConfig } from "@/components/ui/chart"
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
import { Skeleton } from "@/components/ui/skeleton"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import { MessagePill } from "@/components/status-pill"
import { cn } from "@/lib/utils"
import {
  ApiError,
  type Campaign,
  type CampaignStats,
  type CampaignWithStream,
  type Message,
} from "@/lib/api"
import { useServerMessagingApi } from "@/lib/api-p2"
import { recipientHref } from "@/lib/api-p2"
import { relativeTime } from "@/lib/api-p1"
import { renderMustache, sampleModel } from "@/lib/api-p3"
import { STAT_COLORS } from "@/lib/api-p4"
import {
  BlockEditor,
  blocksToHtml,
  htmlToBlocks,
  STARTER_BLOCKS,
  type Block,
} from "./template-blocks"

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
// The body authoring mode: the visual block editor, raw HTML, or plain text —
// the same segmented switch the template editor uses.
type EditorMode = "editor" | "html" | "text"

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
/// stream), name, from, subject, the block-based email body, and a "When to
/// send" control. The body reuses the template block editor with an Editor /
/// HTML / Plain Text switch and a live preview.
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
  // A new campaign starts in the visual editor with the starter blocks; the
  // HTML body stays in sync with the blocks (they serialize to it).
  const starter = useMemo(() => STARTER_BLOCKS(), [])
  const [mode, setMode] = useState<EditorMode>("editor")
  const [blocks, setBlocks] = useState<Block[]>(starter)
  const [htmlBody, setHtmlBody] = useState(() => blocksToHtml(starter))
  const [textBody, setTextBody] = useState("")
  const [sendMode, setSendMode] = useState<SendMode>("draft")
  const [scheduleLocal, setScheduleLocal] = useState("")
  // Seed the form once the edited campaign arrives.
  const [seeded, setSeeded] = useState(false)
  if (editing && !seeded && existing.data) {
    const c = existing.data.campaign
    setStream(c.stream?.permalink ?? "")
    setName(c.name ?? "")
    setFrom(c.from)
    setSubject(c.subject)
    // Choose the body authoring mode from the stored HTML: a block-authored
    // body carries a marker we can parse back into blocks (→ visual editor);
    // arbitrary HTML opens raw; an empty body starts from the starter blocks.
    const parsed = htmlToBlocks(c.html_body)
    if (parsed) {
      setBlocks(parsed)
      setHtmlBody(c.html_body ?? "")
      setMode("editor")
    } else if (c.html_body) {
      setBlocks([])
      setHtmlBody(c.html_body)
      setMode("html")
    } else {
      const st = STARTER_BLOCKS()
      setBlocks(st)
      setHtmlBody(blocksToHtml(st))
      setMode("editor")
    }
    setTextBody(c.text_body ?? "")
    if (c.scheduled_at) {
      setSendMode("schedule")
      setScheduleLocal(isoToLocalInput(c.scheduled_at))
    } else {
      setSendMode("draft")
    }
    setSeeded(true)
  }

  // Editing blocks keeps the saved/previewed HTML in sync (no effect needed).
  function updateBlocks(next: Block[]) {
    setBlocks(next)
    setHtmlBody(blocksToHtml(next))
  }
  function switchMode(next: EditorMode) {
    if (next === mode) return
    if (next === "editor") {
      const parsed = htmlToBlocks(htmlBody)
      if (parsed) {
        setBlocks(parsed)
      } else if (blocks.length > 0) {
        toast("Switching to the editor — block edits will replace the current HTML.")
      } else {
        toast("This HTML wasn’t built with the editor. Adding a block replaces it.")
      }
    }
    setMode(next)
  }

  // Live preview: render the Mustache subset client-side with a sample model so
  // unsaved edits show instantly, exactly like the template editor.
  const model = useMemo(
    () => sampleModel(subject, htmlBody, textBody),
    [subject, htmlBody, textBody],
  )
  const renderedSubject = useMemo(() => renderMustache(subject, model), [subject, model])
  const renderedHtml = useMemo(() => renderMustache(htmlBody, model), [htmlBody, model])
  const renderedText = useMemo(() => renderMustache(textBody, model), [textBody, model])

  const save = useMutation({
    mutationFn: () => {
      const scheduled =
        sendMode === "schedule" && scheduleLocal
          ? { scheduled_at: new Date(scheduleLocal).toISOString() }
          : {}
      if (editing) {
        return api!.campaigns.update(id!, {
          ...(name.trim() ? { name: name.trim() } : {}),
          from,
          subject,
          ...(htmlBody ? { html_body: htmlBody } : {}),
          ...(textBody ? { text_body: textBody } : {}),
          ...scheduled,
        })
      }
      return api!.campaigns.create({
        stream,
        ...(name.trim() ? { name: name.trim() } : {}),
        from,
        subject,
        ...(htmlBody ? { html_body: htmlBody } : {}),
        ...(textBody ? { text_body: textBody } : {}),
        ...scheduled,
        ...(sendMode === "now" ? { send_now: true } : {}),
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
    (sendMode !== "schedule" || scheduleLocal.length > 0)

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
        <div className="grid max-w-2xl gap-4">
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-10 w-full" />
          <Skeleton className="h-64 w-full" />
        </div>
      ) : (
        <div className="grid gap-5">
          {/* Envelope + audience: identity fields in a compact column. */}
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
          </div>

          <div className="border-t" />

          {/* Email body: block editor (left) and live preview (right). */}
          <div className="grid gap-6 lg:grid-cols-2">
            <div className="grid content-start gap-4 lg:border-r lg:pr-6">
              <Tabs value={mode} onValueChange={(v) => switchMode(v as EditorMode)}>
                <div className="flex items-center justify-between gap-2">
                  <Label>Email body</Label>
                  <TabsList>
                    <TabsTrigger value="editor">Editor</TabsTrigger>
                    <TabsTrigger value="html">HTML</TabsTrigger>
                    <TabsTrigger value="text">Plain Text</TabsTrigger>
                  </TabsList>
                </div>

                <TabsContent value="editor" className="mt-2">
                  <BlockEditor blocks={blocks} onChange={updateBlocks} />
                </TabsContent>

                <TabsContent value="html" className="mt-2 grid gap-1.5">
                  <Textarea
                    rows={18}
                    value={htmlBody}
                    onChange={(e) => setHtmlBody(e.target.value)}
                    className="font-mono text-xs"
                    placeholder="<h1>Hi {{ name }}</h1>"
                  />
                  <p className="text-xs text-muted-foreground">
                    Expert mode: write raw, email-safe HTML. Mustache-style{" "}
                    <code className="font-mono">{"{{ variables }}"}</code> are filled from a sample model in the preview.
                  </p>
                </TabsContent>

                <TabsContent value="text" className="mt-2 grid gap-1.5">
                  <Textarea
                    rows={12}
                    value={textBody}
                    onChange={(e) => setTextBody(e.target.value)}
                    className="font-mono text-xs"
                    placeholder="Hi {{ name }}"
                  />
                  <p className="text-xs text-muted-foreground">
                    Plain-text alternative, shown by clients that do not render HTML.
                  </p>
                </TabsContent>
              </Tabs>
            </div>

            {/* Live preview — sticky so it stays in view while the editor scrolls. */}
            <div className="grid content-start gap-3 lg:sticky lg:top-4 lg:self-start">
              <Tabs defaultValue="html">
                <div className="flex items-center justify-between gap-2">
                  <TabsList>
                    <TabsTrigger value="html">Preview</TabsTrigger>
                    <TabsTrigger value="text">Plain text</TabsTrigger>
                  </TabsList>
                  <span className="truncate text-xs text-muted-foreground">
                    {from || "from"} ·{" "}
                    <span className="font-medium text-foreground">{renderedSubject || "—"}</span>
                  </span>
                </div>

                <TabsContent value="html" className="mt-2">
                  {renderedHtml ? (
                    <iframe
                      title="Campaign preview"
                      sandbox=""
                      srcDoc={renderedHtml}
                      className="h-[clamp(24rem,calc(100svh-18rem),40rem)] w-full rounded-md border bg-white"
                    />
                  ) : (
                    <p className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
                      Add a block or some HTML to see the preview.
                    </p>
                  )}
                </TabsContent>

                <TabsContent value="text" className="mt-2">
                  <pre className="h-[clamp(24rem,calc(100svh-18rem),40rem)] overflow-auto whitespace-pre-wrap rounded-md border bg-muted p-3 text-xs">
                    {renderedText || "No plain-text body."}
                  </pre>
                </TabsContent>
              </Tabs>
            </div>
          </div>

          <div className="border-t" />

          <div className="grid max-w-2xl gap-4">
            <div className="grid gap-2">
              <Label>When to send</Label>
              <div className="flex flex-wrap gap-2">
                {sendOptions.map((option) => (
                  <Button
                    key={option.value}
                    type="button"
                    variant={sendMode === option.value ? "default" : "outline"}
                    size="sm"
                    onClick={() => setSendMode(option.value)}
                  >
                    {option.label}
                  </Button>
                ))}
              </div>
              {sendMode === "schedule" && (
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
                    : sendMode === "now"
                      ? "Send campaign"
                      : sendMode === "schedule"
                        ? "Schedule campaign"
                        : "Save draft"}
              </Button>
              <Button variant="ghost" asChild>
                <Link href={base}>Cancel</Link>
              </Button>
            </div>
          </div>
        </div>
      )}
    </Page>
  )
}

// -------------------------------------------------------------- detail

// The engagement donut over the campaign's delivered mail: clicked ⊂ opened ⊂
// delivered, shown as disjoint bands in the same visual language as the
// server-wide Statistics view.
const donutConfig = {
  clicked: { label: "Clicked", theme: STAT_COLORS.clicked },
  opened: { label: "Opened", theme: STAT_COLORS.opened },
  neither: { label: "Not opened", theme: STAT_COLORS.neither },
} satisfies ChartConfig

function EngagementDonut({ stats }: { stats: CampaignStats }) {
  const delivered = stats.delivered
  const clicked = Math.min(stats.clicked, delivered)
  const openedOnly = Math.max(0, Math.min(stats.opened, delivered) - clicked)
  const neither = Math.max(0, delivered - clicked - openedOnly)
  const data = [
    { key: "clicked", label: "Clicked", value: clicked },
    { key: "opened", label: "Opened", value: openedOnly },
    { key: "neither", label: "Not opened", value: neither },
  ].filter((d) => d.value > 0)

  if (delivered === 0) {
    return (
      <p className="text-sm text-muted-foreground">
        Nothing delivered yet. Engagement appears once mail lands.
      </p>
    )
  }

  return (
    <div className="flex flex-wrap items-center gap-6">
      <ChartContainer config={donutConfig} className="aspect-square h-40">
        <PieChart>
          <Pie
            data={data}
            dataKey="value"
            nameKey="label"
            innerRadius="60%"
            outerRadius="100%"
            strokeWidth={2}
            isAnimationActive={false}
          >
            {data.map((d) => (
              <Cell key={d.key} fill={`var(--color-${d.key})`} />
            ))}
          </Pie>
        </PieChart>
      </ChartContainer>
      <dl className="space-y-1.5 text-sm">
        <div className="flex items-center gap-2">
          <span className="size-2.5 rounded-sm" style={{ backgroundColor: "var(--color-clicked)" }} />
          <dt className="w-24 text-muted-foreground">Clicked</dt>
          <dd className="font-medium tabular-nums">{clicked}</dd>
        </div>
        <div className="flex items-center gap-2">
          <span className="size-2.5 rounded-sm" style={{ backgroundColor: "var(--color-opened)" }} />
          <dt className="w-24 text-muted-foreground">Opened</dt>
          <dd className="font-medium tabular-nums">{Math.min(stats.opened, delivered)}</dd>
        </div>
        <div className="flex items-center gap-2">
          <span className="size-2.5 rounded-sm" style={{ backgroundColor: "var(--color-neither)" }} />
          <dt className="w-24 text-muted-foreground">Not opened</dt>
          <dd className="font-medium tabular-nums">{neither}</dd>
        </div>
      </dl>
    </div>
  )
}

function pct(part: number, whole: number): string {
  if (whole <= 0) return "—"
  return `${Math.round((part / whole) * 1000) / 10}%`
}

// The consolidated campaign dashboard: headline counters over the campaign's
// stats plus an engagement donut. Loading shows Skeletons, not text.
function CampaignDashboard({
  campaign,
  stats,
  loading,
}: {
  campaign: CampaignWithStream
  stats: CampaignStats | undefined
  loading: boolean
}) {
  const tiles: { label: string; value: number | undefined; hint?: string }[] = [
    { label: "Recipients", value: stats?.total },
    { label: "Sent", value: stats?.sent },
    { label: "Delivered", value: stats?.delivered, hint: stats ? pct(stats.delivered, stats.sent) : undefined },
    { label: "Opened", value: stats?.opened, hint: stats ? pct(stats.opened, stats.delivered) : undefined },
    { label: "Clicked", value: stats?.clicked, hint: stats ? pct(stats.clicked, stats.delivered) : undefined },
    { label: "Failed", value: stats?.failed },
    { label: "Unsubscribed", value: stats?.unsubscribed },
  ]

  if (loading && !stats) {
    return (
      <div className="grid gap-6">
        <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
          {tiles.map((t) => (
            <Skeleton key={t.label} className="h-20 w-full" />
          ))}
        </div>
        <Skeleton className="h-48 w-full max-w-md" />
      </div>
    )
  }

  return (
    <div className="grid gap-6">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {tiles.map((t) => (
          <Card key={t.label}>
            <CardContent className="p-4">
              <p className="text-xs text-muted-foreground">{t.label}</p>
              <p className="text-2xl font-semibold tabular-nums">{t.value ?? 0}</p>
              {t.hint && t.hint !== "—" && (
                <p className="text-xs text-muted-foreground">{t.hint}</p>
              )}
            </CardContent>
          </Card>
        ))}
      </div>

      <div className="grid gap-6 lg:grid-cols-2">
        <Card>
          <CardContent className="grid gap-3 p-4">
            <p className="text-sm font-medium">Engagement</p>
            {stats ? (
              <EngagementDonut stats={stats} />
            ) : (
              <p className="text-sm text-muted-foreground">No stats yet.</p>
            )}
          </CardContent>
        </Card>

        <Card>
          <CardContent className="grid gap-2 p-4 text-sm">
            <p className="mb-1 text-sm font-medium">Campaign</p>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Status</span>
              <StatusBadge status={campaign.status} />
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Audience</span>
              <span className="truncate">{campaign.stream?.name ?? "—"}</span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">From</span>
              <span className="truncate">{campaign.from}</span>
            </div>
            <div className="flex justify-between gap-4">
              <span className="text-muted-foreground">Progress</span>
              <span className="tabular-nums">
                {campaign.sent}/{campaign.total}
              </span>
            </div>
            {campaign.scheduled_at && (
              <div className="flex justify-between gap-4">
                <span className="text-muted-foreground">Scheduled</span>
                <span>{formatDate(campaign.scheduled_at)}</span>
              </div>
            )}
            {campaign.completed_at && (
              <div className="flex justify-between gap-4">
                <span className="text-muted-foreground">Completed</span>
                <span>{formatDate(campaign.completed_at)}</span>
              </div>
            )}
          </CardContent>
        </Card>
      </div>
    </div>
  )
}

/// One campaign as a tabbed workspace: a consolidated Dashboard, the
/// Recipients it reached, and the individual Messages. Polls while the
/// campaign is scheduled (waiting to fire) or sending (in flight).
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

  // The campaign's messages — filtered exactly by campaign_id (the server
  // messages endpoint now supports the filter and exposes campaign_id).
  const messagesQuery = useQuery({
    queryKey: ["sapi-campaign-messages", org, server, id],
    queryFn: () => api!.messages(`?campaign_id=${id}&per_page=100`),
    enabled: api !== null,
    refetchInterval: () => (c?.status === "sending" ? 5000 : false),
  })
  const campaignMessages: Message[] = messagesQuery.data?.messages ?? []

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

  const messageHref = (messageId: number) =>
    `/orgs/${org}/servers/${server}/messaging/${messageId}`

  // Recipients: address + delivery status + timestamp for each message.
  const recipientColumns: ColumnDef<Message>[] = [
    {
      id: "recipient",
      header: "Recipient",
      accessorFn: (m) => m.rcpt_to,
      cell: ({ row }) => (
        <Link
          href={recipientHref(org, server, row.original.rcpt_to)}
          className="block max-w-64 truncate font-medium text-primary underline-offset-2 hover:underline"
        >
          {row.original.rcpt_to}
        </Link>
      ),
    },
    {
      id: "status",
      header: "Delivery",
      accessorFn: (m) => m.status ?? "",
      cell: ({ row }) => <MessagePill message={row.original} />,
    },
    {
      id: "time",
      header: "Time",
      accessorFn: (m) => m.created_at,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span
          className="whitespace-nowrap text-muted-foreground"
          title={formatDate(row.original.created_at)}
        >
          {relativeTime(row.original.created_at)}
        </span>
      ),
    },
  ]

  // Messages: the individual messages of this campaign, mirroring the
  // messaging activity table's columns.
  const messageColumns: ColumnDef<Message>[] = [
    {
      id: "event",
      header: "Event",
      accessorFn: (m) => m.status ?? "",
      cell: ({ row }) => <MessagePill message={row.original} />,
    },
    {
      id: "recipient",
      header: "Recipient",
      accessorFn: (m) => m.rcpt_to,
      cell: ({ row }) => (
        <Link
          href={recipientHref(org, server, row.original.rcpt_to)}
          className="block max-w-48 truncate font-medium text-primary underline-offset-2 hover:underline"
          onClick={(e) => e.stopPropagation()}
        >
          {row.original.rcpt_to}
        </Link>
      ),
    },
    {
      id: "subject",
      header: "Subject",
      accessorFn: (m) => m.subject ?? "",
      cell: ({ row }) => (
        <Link
          href={messageHref(row.original.id)}
          className="block max-w-64 truncate text-left transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.subject ?? "—"}
        </Link>
      ),
    },
    {
      id: "time",
      header: "Time",
      accessorFn: (m) => m.created_at,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span
          className="whitespace-nowrap text-muted-foreground"
          title={formatDate(row.original.created_at)}
        >
          {relativeTime(row.original.created_at)}
        </span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <Button variant="ghost" size="icon" aria-label="View message" asChild>
          <Link href={messageHref(row.original.id)}>
            <ChevronRightIcon className="size-4" />
          </Link>
        </Button>
      ),
    },
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
        <div className="grid gap-6">
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
            {Array.from({ length: 7 }).map((_, i) => (
              <Skeleton key={i} className="h-20 w-full" />
            ))}
          </div>
          <Skeleton className="h-48 w-full max-w-md" />
        </div>
      ) : !c ? (
        <p className="text-sm text-muted-foreground">This campaign could not be loaded.</p>
      ) : (
        <Tabs defaultValue="dashboard" className="gap-4">
          <TabsList>
            <TabsTrigger value="dashboard">Dashboard</TabsTrigger>
            <TabsTrigger value="recipients">Recipients</TabsTrigger>
            <TabsTrigger value="messages">Messages</TabsTrigger>
          </TabsList>

          <TabsContent value="dashboard">
            <CampaignDashboard campaign={c} stats={s} loading={query.isLoading} />
          </TabsContent>

          <TabsContent value="recipients">
            <DataTable
              columns={recipientColumns}
              data={campaignMessages}
              loading={messagesQuery.isLoading}
              searchKeys={["rcpt_to"]}
              searchPlaceholder="Search recipients…"
              emptyText="No recipients yet. They appear once the campaign starts sending."
              initialPageSize={20}
            />
          </TabsContent>

          <TabsContent value="messages">
            <DataTable
              columns={messageColumns}
              data={campaignMessages}
              loading={messagesQuery.isLoading}
              searchKeys={["rcpt_to", "subject"]}
              searchPlaceholder="Search messages…"
              emptyText="No messages yet. They appear once the campaign starts sending."
              initialPageSize={20}
            />
          </TabsContent>
        </Tabs>
      )}
    </Page>
  )
}
