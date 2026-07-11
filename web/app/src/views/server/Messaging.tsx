"use client"

// Messaging: everything that talks to the per-server API
// (`X-Server-API-Key`). The key is picked from the server's API
// credentials — no credential, no messaging.

import { createContext, useContext, useEffect, useMemo, useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { usePathname, useRouter } from "next/navigation"
import {
  CircleCheckIcon,
  FileTextIcon,
  InboxIcon,
  KeyRoundIcon,
  LayersIcon,
  MailIcon,
  MoreVerticalIcon,
  PlusIcon,
  RefreshCwIcon,
  ScrollTextIcon,
  SearchIcon,
} from "lucide-react"
import { toast } from "sonner"
import { CopyButton, formatDate, PageHeader } from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { FormDialog } from "@/components/form-dialog"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import {
  adminApi,
  ApiError,
  serverApi,
  type InsightCheck,
  type Message,
  type Template,
} from "@/lib/api"
import {
  httpStatusPillClass,
  messageEventPill,
  parseRawEmail,
  relativeTime,
  serverApiP1,
  type ApiRequestEntry,
  type ParsedEmail,
} from "@/lib/api-p1"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

/// A lifecycle event pill using the shared color semantics (no
/// status-pill.tsx component — classes on the existing Badge).
function EventPill({ message }: { message: Pick<Message, "held" | "status"> }) {
  const { label, className } = messageEventPill(message)
  return (
    <Badge variant="outline" className={className}>
      {label}
    </Badge>
  )
}

type Api = ReturnType<typeof serverApi>

// ---------------------------------------------------------------- send

export function Send({ api }: { api: Api }) {
  const [from, setFrom] = useState("")
  const [to, setTo] = useState("")
  const [subject, setSubject] = useState("")
  const [textBody, setTextBody] = useState("")
  const [htmlBody, setHtmlBody] = useState("")
  const [templatePermalink, setTemplatePermalink] = useState("none")
  const [templateModel, setTemplateModel] = useState("{}")
  const [result, setResult] = useState<string | null>(null)
  const templates = useQuery({ queryKey: ["sapi-templates"], queryFn: api.templates.list })

  const send = useMutation({
    mutationFn: async () => {
      const recipients = to.split(",").map((address) => address.trim()).filter(Boolean)
      if (templatePermalink !== "none") {
        let model: unknown = {}
        try {
          model = JSON.parse(templateModel || "{}")
        } catch {
          throw new ApiError("ValidationError", "The template model is not valid JSON", 422)
        }
        return api.sendWithTemplate({
          from,
          to: recipients,
          template: templatePermalink,
          template_model: model,
        })
      }
      return api.send({
        from,
        to: recipients,
        subject,
        ...(textBody ? { text_body: textBody } : {}),
        ...(htmlBody ? { html_body: htmlBody } : {}),
      })
    },
    onSuccess: (data) => {
      setResult(`Queued as message #${(data as { message_id: number }).message_id}`)
      toast.success("Message queued")
    },
    onError: (err) => errorToast(err, "Sending failed"),
  })

  return (
    <Card className="max-w-2xl">
      <CardHeader>
        <CardTitle className="text-base">Send a message</CardTitle>
      </CardHeader>
      <CardContent className="grid gap-4">
        <div className="grid grid-cols-2 gap-2">
          <div className="grid gap-2">
            <Label>From</Label>
            <Input value={from} onChange={(e) => setFrom(e.target.value)} placeholder="hello@yourdomain.com" />
          </div>
          <div className="grid gap-2">
            <Label>To (comma-separated)</Label>
            <Input value={to} onChange={(e) => setTo(e.target.value)} placeholder="a@x.com, b@y.com" />
          </div>
        </div>
        <div className="grid gap-2">
          <Label>Template (optional)</Label>
          <Select value={templatePermalink} onValueChange={setTemplatePermalink}>
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="none">No template — compose below</SelectItem>
              {templates.data?.templates
                .filter((template) => !template.archived)
                .map((template) => (
                  <SelectItem key={template.id} value={template.permalink}>
                    {template.name}
                  </SelectItem>
                ))}
            </SelectContent>
          </Select>
        </div>
        {templatePermalink === "none" ? (
          <>
            <div className="grid gap-2">
              <Label>Subject</Label>
              <Input value={subject} onChange={(e) => setSubject(e.target.value)} />
            </div>
            <div className="grid gap-2">
              <Label>Text body</Label>
              <Textarea rows={5} value={textBody} onChange={(e) => setTextBody(e.target.value)} />
            </div>
            <div className="grid gap-2">
              <Label>HTML body (optional)</Label>
              <Textarea rows={5} value={htmlBody} onChange={(e) => setHtmlBody(e.target.value)} />
            </div>
          </>
        ) : (
          <div className="grid gap-2">
            <Label>Template model (JSON)</Label>
            <Textarea
              rows={5}
              value={templateModel}
              onChange={(e) => setTemplateModel(e.target.value)}
              className="font-mono text-xs"
            />
          </div>
        )}
        {result && <p className="text-sm text-muted-foreground">{result}</p>}
        <Button
          className="justify-self-start"
          onClick={() => send.mutate()}
          disabled={send.isPending || !from.includes("@") || !to.includes("@")}
        >
          {send.isPending ? "Sending…" : "Send"}
        </Button>
      </CardContent>
    </Card>
  )
}

// ------------------------------------------------------------ messages

function statusBadge(message: Message) {
  if (message.held) return <Badge variant="destructive">held</Badge>
  switch (message.status) {
    case "Sent":
      return <Badge>sent</Badge>
    case "SoftFail":
      return <Badge variant="secondary">soft fail</Badge>
    case "HardFail":
      return <Badge variant="destructive">hard fail</Badge>
    case "Bounced":
      return <Badge variant="destructive">bounced</Badge>
    default:
      return <Badge variant="outline">{message.status ?? "pending"}</Badge>
  }
}

const SHARE_EXPIRY_OPTIONS = [
  { value: "24", label: "24 hours" },
  { value: "48", label: "48 hours" },
  { value: "168", label: "7 days" },
]

/// "Share" (kebab action of the message detail): pick an expiry, generate
/// the public link, copy it. The URL is shown exactly once.
function ShareDialog({ api, id, onClose }: { api: Api; id: number; onClose: () => void }) {
  const [expiry, setExpiry] = useState("48")
  const [result, setResult] = useState<{ url: string; expires_at: string } | null>(null)

  const generate = useMutation({
    mutationFn: () => api.share(id, Number(expiry)),
    onSuccess: (share) => setResult(share),
    onError: (err) => errorToast(err, "Could not create the share link"),
  })

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Share message #{id}</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          Anyone with the link can view this message — including its content and
          delivery timeline — until the link expires. No account needed.
        </p>
        {result ? (
          <div className="grid gap-2">
            <Label>Share link</Label>
            <div className="flex items-center gap-2">
              <code className="min-w-0 flex-1 break-all rounded bg-muted px-2 py-1 text-xs">
                {result.url}
              </code>
              <CopyButton value={result.url} />
            </div>
            <p className="text-xs text-muted-foreground">
              Expires {formatDate(result.expires_at)}.
            </p>
          </div>
        ) : (
          <div className="grid gap-2">
            <Label>Link expires after</Label>
            <Select value={expiry} onValueChange={setExpiry}>
              <SelectTrigger className="w-40">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SHARE_EXPIRY_OPTIONS.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {result ? "Done" : "Cancel"}
          </Button>
          {!result && (
            <Button onClick={() => generate.mutate()} disabled={generate.isPending}>
              {generate.isPending ? "Generating…" : "Generate link"}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

function InsightRow({ check }: { check: InsightCheck }) {
  return (
    <div className="flex items-start gap-2 rounded-md border p-2">
      <Badge
        variant={check.status === "ok" ? "default" : "destructive"}
        className="mt-0.5 shrink-0"
      >
        {check.status}
      </Badge>
      <div className="min-w-0">
        <p className="text-sm font-medium">{check.title}</p>
        <p className="text-xs text-muted-foreground">{check.detail}</p>
      </div>
    </div>
  )
}

/// The "Insights" tab: DOING GREAT / IMPROVE sections over the server-side
/// deliverability checks, with the generation timestamp underneath.
function InsightsPanel({ api, id }: { api: Api; id: number }) {
  const insights = useQuery({
    queryKey: ["sapi-insights", id],
    queryFn: () => api.insights(id),
  })
  if (insights.isLoading) {
    return <p className="text-sm text-muted-foreground">Analyzing…</p>
  }
  const data = insights.data
  if (!data) {
    return <p className="text-sm text-muted-foreground">Insights are unavailable.</p>
  }
  const great = data.checks.filter((check) => check.status === "ok")
  const improve = data.checks.filter((check) => check.status === "warning")
  return (
    <div className="grid gap-4">
      {improve.length > 0 && (
        <div className="grid gap-2">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            Improve
          </h3>
          {improve.map((check) => (
            <InsightRow key={check.id} check={check} />
          ))}
        </div>
      )}
      {great.length > 0 && (
        <div className="grid gap-2">
          <h3 className="text-xs font-semibold uppercase tracking-wide text-muted-foreground">
            Doing great
          </h3>
          {great.map((check) => (
            <InsightRow key={check.id} check={check} />
          ))}
        </div>
      )}
      <p className="text-xs text-muted-foreground">
        Report generated on {formatDate(data.generated_at)}.
      </p>
    </div>
  )
}

/// The delivery timestamp, tolerant of the API's `created_at` field name.
function deliveryTime(delivery: { timestamp?: string; created_at?: string }): string {
  return delivery.timestamp ?? delivery.created_at ?? ""
}

type TimelineStage = {
  key: string
  label: string
  at: string | null
  reached: boolean
  tone: "done" | "bad" | "pending"
}

/// The horizontal lifecycle timeline: Sent → Delivered → Opened → Clicked,
/// with the Delivered node flipping to Bounced / Held on failure.
function EventTimeline({
  message,
  deliveries,
  opens,
  clicks,
}: {
  message: Message
  deliveries: { status: string; timestamp?: string; created_at?: string }[]
  opens: { created_at: string }[]
  clicks: { created_at: string }[]
}) {
  const delivered = deliveries.find((d) => d.status === "Sent" || d.status === "Delivered")
  const firstOpen = [...opens].sort((a, b) => a.created_at.localeCompare(b.created_at))[0]
  const firstClick = [...clicks].sort((a, b) => a.created_at.localeCompare(b.created_at))[0]
  const failed =
    message.status === "Bounced" ||
    message.status === "HardFail" ||
    message.bounce === true
  const lastFail = deliveries
    .filter((d) => d.status !== "Sent" && d.status !== "Delivered")
    .slice(-1)[0]

  const middle: TimelineStage = message.held
    ? { key: "held", label: "Held", at: message.created_at, reached: true, tone: "bad" }
    : failed
      ? {
          key: "bounced",
          label: "Bounced",
          at: lastFail ? deliveryTime(lastFail) : message.created_at,
          reached: true,
          tone: "bad",
        }
      : {
          key: "delivered",
          label: "Delivered",
          at: delivered ? deliveryTime(delivered) : null,
          reached: !!delivered,
          tone: "done",
        }

  const stages: TimelineStage[] = [
    { key: "sent", label: "Sent", at: message.created_at, reached: true, tone: "done" },
    middle,
    {
      key: "opened",
      label: "Opened",
      at: firstOpen?.created_at ?? null,
      reached: !!firstOpen,
      tone: "done",
    },
    {
      key: "clicked",
      label: "Clicked",
      at: firstClick?.created_at ?? null,
      reached: !!firstClick,
      tone: "done",
    },
  ]

  return (
    <ol className="flex items-start gap-1 overflow-x-auto py-2">
      {stages.map((stage, index) => (
        <li key={stage.key} className="flex min-w-24 flex-1 items-start gap-1">
          <div className="flex flex-col items-center gap-1 text-center">
            <span
              className={`flex size-6 items-center justify-center rounded-full text-[10px] font-semibold ${
                !stage.reached
                  ? "border border-dashed border-border text-muted-foreground"
                  : stage.tone === "bad"
                    ? "bg-red-600/15 text-red-700 dark:text-red-400"
                    : "bg-green-600/15 text-green-700 dark:text-green-400"
              }`}
            >
              {stage.reached ? "✓" : index + 1}
            </span>
            <span
              className={`text-xs font-medium ${stage.reached ? "" : "text-muted-foreground"}`}
            >
              {stage.label}
            </span>
            <span className="whitespace-nowrap text-[10px] text-muted-foreground">
              {stage.at ? relativeTime(stage.at) : "—"}
            </span>
          </div>
          {index < stages.length - 1 && (
            <span
              className={`mt-3 h-px flex-1 ${
                stages[index + 1].reached ? "bg-green-600/40" : "bg-border"
              }`}
            />
          )}
        </li>
      ))}
    </ol>
  )
}

/// A copyable label:value cell of the metadata grid.
function MetaRow({
  label,
  value,
  copy,
}: {
  label: string
  value: string | null | undefined
  copy?: boolean
}) {
  return (
    <>
      <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        {label}
      </span>
      <span className="flex min-w-0 items-center gap-1 break-all">
        <span className="min-w-0 flex-1">{value || "—"}</span>
        {copy && value && <CopyButton value={value} />}
      </span>
    </>
  )
}

/// Initial `?tab=` value (client-only; no useSearchParams so no Suspense
/// boundary is required and the build stays static-safe).
function initialTab(fallback: string): string {
  if (typeof window === "undefined") return fallback
  return new URLSearchParams(window.location.search).get("tab") || fallback
}

function MessageDetail({ api, id, onClose }: { api: Api; id: number; onClose: () => void }) {
  const p1 = useMessagingApiP1()
  const message = useQuery({ queryKey: ["sapi-message", id], queryFn: () => api.message(id) })
  const deliveries = useQuery({
    queryKey: ["sapi-deliveries", id],
    queryFn: () => api.deliveries(id),
  })
  const opens = useQuery({ queryKey: ["p1-opens", id], queryFn: () => p1.opens(id) })
  const clicks = useQuery({ queryKey: ["p1-clicks", id], queryFn: () => p1.clicks(id) })
  const insights = useQuery({
    queryKey: ["sapi-insights", id],
    queryFn: () => api.insights(id),
  })
  // Raw MIME → display bodies, headers, attachments. 404 in privacy mode.
  const raw = useQuery({
    queryKey: ["p1-raw", id],
    queryFn: () => p1.rawMime(id),
    retry: false,
  })
  const parsed: ParsedEmail | null = useMemo(
    () => (raw.data ? parseRawEmail(raw.data.raw_message) : null),
    [raw.data],
  )
  const privacyMode =
    raw.error instanceof ApiError && raw.error.code === "NotAvailable"

  const [sharing, setSharing] = useState(false)
  const [tab, setTab] = useState(() => initialTab("preview"))

  // Reflect the active tab in the URL for a shareable deep link.
  useEffect(() => {
    if (typeof window === "undefined") return
    const url = new URL(window.location.href)
    url.searchParams.set("tab", tab)
    window.history.replaceState(window.history.state, "", url)
  }, [tab])

  const warnings =
    insights.data?.checks.filter((check) => check.status === "warning").length ?? 0
  const m = message.data?.message
  const html = parsed?.htmlBody ?? null
  const text = parsed?.textBody ?? null
  const replyTo = parsed?.headers["reply-to"] ?? null
  const attachments = parsed?.attachments ?? []

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <div className="flex items-start justify-between gap-2 pr-6">
            <div className="min-w-0">
              <DialogTitle className="truncate">{m?.subject || `Message #${id}`}</DialogTitle>
              <p className="mt-0.5 flex items-center gap-1.5 text-sm text-muted-foreground">
                To {m?.rcpt_to ?? "…"} {m && <EventPill message={m} />}
              </p>
            </div>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button variant="ghost" size="icon" aria-label="Message actions">
                  <MoreVerticalIcon className="size-4" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuItem onClick={() => setSharing(true)}>Share email…</DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </DialogHeader>
        {m && (
          <div className="space-y-4">
            <div className="grid grid-cols-[5.5rem_1fr] gap-x-3 gap-y-1.5 text-sm sm:grid-cols-[5.5rem_1fr_5.5rem_1fr]">
              <MetaRow label="From" value={m.mail_from} />
              <MetaRow label="Subject" value={m.subject} />
              <MetaRow label="To" value={m.rcpt_to} />
              <MetaRow label="ID" value={m.message_id ?? String(m.id)} copy />
              <MetaRow label="Reply-To" value={replyTo} />
              <MetaRow
                label="Attach."
                value={
                  attachments.length
                    ? attachments.map((a) => a.filename ?? a.contentType).join(", ")
                    : "None"
                }
              />
            </div>

            <div className="rounded-md border bg-muted/30 px-2">
              <EventTimeline
                message={m}
                deliveries={deliveries.data?.deliveries ?? []}
                opens={opens.data?.opens ?? []}
                clicks={clicks.data?.clicks ?? []}
              />
            </div>

            <Tabs value={tab} onValueChange={setTab}>
              <TabsList className="mb-2 flex-wrap">
                <TabsTrigger value="preview">Preview</TabsTrigger>
                <TabsTrigger value="text">Plain Text</TabsTrigger>
                <TabsTrigger value="html">HTML</TabsTrigger>
                <TabsTrigger value="raw">Raw</TabsTrigger>
                <TabsTrigger value="insights">
                  Insights
                  {warnings > 0 && (
                    <Badge variant="destructive" className="ml-1 px-1.5">
                      {warnings}
                    </Badge>
                  )}
                </TabsTrigger>
              </TabsList>

              <div className="max-h-[55svh] overflow-y-auto pr-1">
                <TabsContent value="preview">
                  {privacyMode ? (
                    <PrivacyNote />
                  ) : html ? (
                    <iframe
                      title="Message preview"
                      sandbox=""
                      srcDoc={html}
                      className="h-[45svh] w-full rounded-md border bg-white"
                    />
                  ) : (
                    <p className="text-sm text-muted-foreground">
                      {raw.isLoading ? "Loading…" : "No HTML part to preview."}
                    </p>
                  )}
                </TabsContent>
                <TabsContent value="text">
                  {privacyMode ? (
                    <PrivacyNote />
                  ) : text ? (
                    <pre className="overflow-auto whitespace-pre-wrap rounded-md bg-muted p-3 text-xs">
                      {text}
                    </pre>
                  ) : (
                    <p className="text-sm text-muted-foreground">
                      {raw.isLoading ? "Loading…" : "No plain-text part."}
                    </p>
                  )}
                </TabsContent>
                <TabsContent value="html">
                  {privacyMode ? (
                    <PrivacyNote />
                  ) : html ? (
                    <div className="relative">
                      <div className="absolute right-2 top-2">
                        <CopyButton value={html} />
                      </div>
                      <pre className="overflow-auto rounded-md bg-muted p-3 text-xs">{html}</pre>
                    </div>
                  ) : (
                    <p className="text-sm text-muted-foreground">
                      {raw.isLoading ? "Loading…" : "No HTML part."}
                    </p>
                  )}
                </TabsContent>
                <TabsContent value="raw">
                  <div className="relative">
                    <div className="absolute right-2 top-2">
                      <CopyButton value={JSON.stringify(m, null, 2)} />
                    </div>
                    <pre className="overflow-auto rounded-md bg-muted p-3 text-xs">
                      {JSON.stringify(m, null, 2)}
                    </pre>
                  </div>
                </TabsContent>
                <TabsContent value="insights">
                  <InsightsPanel api={api} id={id} />
                </TabsContent>
              </div>
            </Tabs>
          </div>
        )}
        {sharing && <ShareDialog api={api} id={id} onClose={() => setSharing(false)} />}
      </DialogContent>
    </Dialog>
  )
}

function PrivacyNote() {
  return (
    <p className="text-sm text-muted-foreground">
      This server runs in privacy mode — message content is not retained, so there is nothing
      to show here.
    </p>
  )
}

const TIME_RANGES = [
  { value: "all", label: "All time", ms: null as number | null },
  { value: "24h", label: "Last 24h", ms: 86_400_000 },
  { value: "7d", label: "Last 7 days", ms: 7 * 86_400_000 },
  { value: "30d", label: "Last 30 days", ms: 30 * 86_400_000 },
]

const STATUS_FILTERS = [
  { value: "all", label: "Any status" },
  { value: "Sent", label: "Delivered" },
  { value: "Pending", label: "Queued" },
  { value: "Held", label: "Held" },
  { value: "Bounced", label: "Bounced" },
  { value: "SoftFail", label: "Soft fail" },
  { value: "HardFail", label: "Hard fail" },
]

/// Activity — the event-oriented message stream (masterplan §4.2): one
/// row per message with its lifecycle pill, recipient (links to the
/// message detail — recipient detail lands in P2), subject, tag and a
/// relative time. Omni-search over sender/subject/recipient/tag plus the
/// Time × Status × Tag × Stream filter row.
export function Messages({ api }: { api: Api }) {
  const p1 = useMessagingApiP1()
  const pathname = usePathname() ?? ""
  const messagingBase = pathname.replace(/\/messages$/, "")
  const [scope, setScope] = useState("outgoing")
  const [query, setQuery] = useState("")
  const [status, setStatus] = useState("all")
  const [tag, setTag] = useState("all")
  const [stream, setStream] = useState("all")
  const [range, setRange] = useState("all")
  const [selected, setSelected] = useState<number | null>(null)

  const tags = useQuery({ queryKey: ["p1-tags"], queryFn: p1.tags })
  const streams = useQuery({ queryKey: ["sapi-streams"], queryFn: api.streams.list })

  const params = useMemo(() => {
    const q = new URLSearchParams({ scope, per_page: "50" })
    if (query) q.set("query", query)
    if (status !== "all") q.set("status", status)
    if (tag !== "all") q.set("tag", tag)
    if (stream !== "all") q.set("stream", stream)
    return `?${q.toString()}`
  }, [scope, query, status, tag, stream])

  const messages = useQuery({
    queryKey: ["sapi-messages", params],
    queryFn: () => api.messages(params),
  })

  // Time window is not a server-side filter on /messages, so it applies
  // client-side to the fetched page.
  const cutoff = TIME_RANGES.find((r) => r.value === range)?.ms ?? null
  const rows = useMemo(() => {
    const now = new Date().getTime()
    return (messages.data?.messages ?? []).filter(
      (m) => cutoff == null || now - new Date(m.created_at).getTime() <= cutoff,
    )
  }, [messages.data, cutoff])
  const hasFilters = query || status !== "all" || tag !== "all" || stream !== "all" || range !== "all"

  return (
    <div>
      <div className="mb-3 flex items-center gap-2">
        <div className="relative flex-1">
          <SearchIcon className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="pl-8"
            placeholder="Search sender, subject, recipient, tag…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <Button variant="outline" size="icon" onClick={() => messages.refetch()}>
          <RefreshCwIcon className="size-4" />
        </Button>
      </div>
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <Select value={scope} onValueChange={setScope}>
          <SelectTrigger size="sm" className="w-32">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="outgoing">Outgoing</SelectItem>
            <SelectItem value="incoming">Incoming</SelectItem>
          </SelectContent>
        </Select>
        <Select value={range} onValueChange={setRange}>
          <SelectTrigger size="sm" className="w-36">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {TIME_RANGES.map((r) => (
              <SelectItem key={r.value} value={r.value}>
                {r.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={status} onValueChange={setStatus}>
          <SelectTrigger size="sm" className="w-36">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {STATUS_FILTERS.map((s) => (
              <SelectItem key={s.value} value={s.value}>
                {s.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={tag} onValueChange={setTag}>
          <SelectTrigger size="sm" className="w-36">
            <SelectValue placeholder="Any tag" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">Any tag</SelectItem>
            {tags.data?.tags.map((t) => (
              <SelectItem key={t.tag} value={t.tag}>
                {t.tag} ({t.count})
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={stream} onValueChange={setStream}>
          <SelectTrigger size="sm" className="w-36">
            <SelectValue placeholder="Any stream" />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">Any stream</SelectItem>
            {streams.data?.streams
              .filter((s) => !s.archived)
              .map((s) => (
                <SelectItem key={s.id} value={s.permalink}>
                  {s.name}
                </SelectItem>
              ))}
          </SelectContent>
        </Select>
      </div>
      {rows.length === 0 ? (
        hasFilters ? (
          <EmptyState
            icon={SearchIcon}
            title="No events match"
            description="Try a broader search, a wider time range, or clear a filter."
          />
        ) : (
          <EmptyState
            icon={MailIcon}
            title="No activity yet"
            description="Send your first message and watch every delivery, open and click stream in here."
            action={{ label: "Send a message", href: messagingBase }}
          />
        )
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="w-28">Event</TableHead>
              <TableHead>Recipient</TableHead>
              <TableHead>Subject</TableHead>
              <TableHead className="w-32">Tag</TableHead>
              <TableHead className="w-24 text-right">Time</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((message) => (
              <TableRow
                key={message.id}
                className="cursor-pointer"
                onClick={() => setSelected(message.id)}
              >
                <TableCell>
                  <EventPill message={message} />
                </TableCell>
                <TableCell className="max-w-48 truncate">
                  <span className="font-medium text-primary underline-offset-2 hover:underline">
                    {message.rcpt_to}
                  </span>
                </TableCell>
                <TableCell className="max-w-64 truncate">{message.subject ?? "—"}</TableCell>
                <TableCell>
                  {message.tag ? (
                    <Badge variant="secondary" className="font-normal">
                      {message.tag}
                    </Badge>
                  ) : (
                    <span className="text-muted-foreground">—</span>
                  )}
                </TableCell>
                <TableCell
                  className="whitespace-nowrap text-right text-muted-foreground"
                  title={formatDate(message.created_at)}
                >
                  {relativeTime(message.created_at)}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {selected !== null && (
        <MessageDetail api={api} id={selected} onClose={() => setSelected(null)} />
      )}
    </div>
  )
}

// ------------------------------------------------------------ API logs

const LOG_METHODS = ["all", "GET", "POST", "PATCH", "DELETE"]
const LOG_STATUS_CLASSES = [
  { value: "all", label: "Any status" },
  { value: "2xx", label: "2xx success" },
  { value: "3xx", label: "3xx redirect" },
  { value: "4xx", label: "4xx client" },
  { value: "5xx", label: "5xx server" },
]

function LogStatusPill({ code }: { code: number }) {
  return (
    <Badge variant="outline" className={httpStatusPillClass(code)}>
      {code}
    </Badge>
  )
}

/// The API request-log view (masterplan §4.8/Resend `/logs`): the server's
/// own request log with Endpoint / Method / Status (traffic-light pill) /
/// Time, filterable by time range, method and status class.
export function LogsView() {
  const p1 = useMessagingApiP1()
  const [method, setMethod] = useState("all")
  const [status, setStatus] = useState("all")
  const [range, setRange] = useState("24h")

  const params = useMemo(() => {
    const q = new URLSearchParams({ per_page: "100" })
    if (method !== "all") q.set("method", method)
    if (status !== "all") q.set("status", status)
    const ms = TIME_RANGES.find((r) => r.value === range)?.ms
    if (ms) q.set("from", new Date(new Date().getTime() - ms).toISOString())
    return `?${q.toString()}`
  }, [method, status, range])

  const logs = useQuery({
    queryKey: ["p1-logs", params],
    queryFn: () => p1.logs(params),
    refetchInterval: 30_000,
  })
  const rows: ApiRequestEntry[] = logs.data?.requests ?? []
  const hasFilters = method !== "all" || status !== "all"

  return (
    <div>
      <PageHeader
        title="API request log"
        description="Every authenticated call to this server's API — method, endpoint, status and latency."
      />
      <div className="mb-4 flex flex-wrap items-center gap-2">
        <Select value={range} onValueChange={setRange}>
          <SelectTrigger size="sm" className="w-36">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {TIME_RANGES.filter((r) => r.value !== "all").map((r) => (
              <SelectItem key={r.value} value={r.value}>
                {r.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={method} onValueChange={setMethod}>
          <SelectTrigger size="sm" className="w-32">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {LOG_METHODS.map((m) => (
              <SelectItem key={m} value={m}>
                {m === "all" ? "Any method" : m}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Select value={status} onValueChange={setStatus}>
          <SelectTrigger size="sm" className="w-40">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {LOG_STATUS_CLASSES.map((s) => (
              <SelectItem key={s.value} value={s.value}>
                {s.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Button variant="outline" size="icon" onClick={() => logs.refetch()}>
          <RefreshCwIcon className="size-4" />
        </Button>
      </div>
      {rows.length === 0 ? (
        <EmptyState
          icon={ScrollTextIcon}
          title={hasFilters ? "No requests match" : "No requests logged yet"}
          description={
            hasFilters
              ? "Widen the time range or clear a filter."
              : "Calls to this server's messaging API show up here as soon as they arrive."
          }
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead className="w-20">Method</TableHead>
              <TableHead>Endpoint</TableHead>
              <TableHead className="w-20">Status</TableHead>
              <TableHead className="w-24 text-right">Latency</TableHead>
              <TableHead className="w-24 text-right">Time</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((entry) => (
              <TableRow key={entry.id}>
                <TableCell>
                  <Badge variant="outline" className="font-mono text-[10px]">
                    {entry.method}
                  </Badge>
                </TableCell>
                <TableCell className="max-w-80 truncate font-mono text-xs">{entry.path}</TableCell>
                <TableCell>
                  <LogStatusPill code={entry.status_code} />
                </TableCell>
                <TableCell className="text-right tabular-nums text-muted-foreground">
                  {entry.duration_ms}ms
                </TableCell>
                <TableCell
                  className="whitespace-nowrap text-right text-muted-foreground"
                  title={formatDate(entry.created_at)}
                >
                  {relativeTime(entry.created_at)}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  )
}

// --------------------------------------------------------------- queue

export function InboundQueue({ api }: { api: Api }) {
  const queryClient = useQueryClient()
  const inbound = useQuery({
    queryKey: ["sapi-inbound"],
    queryFn: () => api.inbound("?per_page=50"),
  })
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["sapi-inbound"] })

  return (
    <div>
      <PageHeader
        title="Inbound & held messages"
        description="Retry failed inbound deliveries or bypass holds."
      />
      {inbound.data?.messages.length === 0 ? (
        <EmptyState
          icon={InboxIcon}
          title="Nothing waiting"
          description="Failed inbound deliveries and held messages show up here for retry or bypass."
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>#</TableHead>
              <TableHead>To</TableHead>
              <TableHead>Subject</TableHead>
              <TableHead>Status</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {inbound.data?.messages.map((message) => (
              <TableRow key={message.id}>
                <TableCell className="text-muted-foreground">{message.id}</TableCell>
                <TableCell>{message.rcpt_to}</TableCell>
                <TableCell className="max-w-64 truncate">{message.subject ?? "—"}</TableCell>
                <TableCell>{statusBadge(message)}</TableCell>
                <TableCell className="space-x-2 text-right">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={async () => {
                      try {
                        await api.inboundRetry(message.id)
                        invalidate()
                        toast.success("Requeued")
                      } catch (err) {
                        errorToast(err, "Retry failed")
                      }
                    }}
                  >
                    Retry
                  </Button>
                  {message.held && (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={async () => {
                        try {
                          await api.inboundBypass(message.id)
                          invalidate()
                          toast.success("Hold bypassed")
                        } catch (err) {
                          errorToast(err, "Bypass failed")
                        }
                      }}
                    >
                      Bypass hold
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </div>
  )
}

// --------------------------------------------------------------- stats

export function StatsView({ api }: { api: Api }) {
  const stats = useQuery({ queryKey: ["sapi-stats"], queryFn: api.stats, refetchInterval: 15_000 })
  const bounces = useQuery({ queryKey: ["sapi-bounces"], queryFn: api.bounces })
  const s = stats.data?.stats

  const tiles: [string, number | undefined][] = [
    ["Total", s?.total],
    ["Outgoing", s?.outgoing],
    ["Incoming", s?.incoming],
    ["Sent", s?.sent],
    ["Pending", s?.pending],
    ["Held", s?.held],
    ["Bounced", s?.bounced],
    ["Soft fails", s?.soft_fail],
    ["Hard fails", s?.hard_fail],
    ["Opens", s?.opens],
    ["Clicks", s?.clicks],
  ]

  return (
    <div className="space-y-6">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 lg:grid-cols-6">
        {tiles.map(([label, value]) => (
          <Card key={label}>
            <CardContent className="p-4">
              <p className="text-xs text-muted-foreground">{label}</p>
              <p className="text-2xl font-semibold tabular-nums">{value ?? "—"}</p>
            </CardContent>
          </Card>
        ))}
      </div>
      <div>
        <h3 className="mb-2 font-medium">Recent bounces</h3>
        {bounces.data?.bounces.length === 0 ? (
          <EmptyState
            icon={CircleCheckIcon}
            title="No bounces recorded"
            description="Your bounce list is clean — deliverability is looking good."
          />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>To</TableHead>
                <TableHead>Subject</TableHead>
                <TableHead>Created</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {bounces.data?.bounces.map((message) => (
                <TableRow key={message.id}>
                  <TableCell>{message.rcpt_to}</TableCell>
                  <TableCell className="max-w-64 truncate">{message.subject ?? "—"}</TableCell>
                  <TableCell className="text-muted-foreground">
                    {formatDate(message.created_at)}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>
    </div>
  )
}

// ------------------------------------------------------------- streams

export function Streams({ api }: { api: Api }) {
  const queryClient = useQueryClient()
  const streams = useQuery({ queryKey: ["sapi-streams"], queryFn: api.streams.list })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [type, setType] = useState("transactional")
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["sapi-streams"] })

  const create = useMutation({
    mutationFn: () => api.streams.create({ name, stream_type: type }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setName("")
    },
    onError: (err) => errorToast(err, "Could not create the stream"),
  })

  return (
    <div>
      <PageHeader
        title="Message streams"
        description="Group outgoing mail (transactional / broadcast) for stats and policies."
        action={
          <Button size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> New stream
          </Button>
        }
      />
      {streams.isSuccess && streams.data.streams.length === 0 ? (
        <EmptyState
          icon={LayersIcon}
          title="No streams yet"
          description="Streams separate transactional from broadcast mail for cleaner stats and policies."
          action={{ label: "New stream", onClick: () => setOpen(true) }}
        />
      ) : (
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Permalink</TableHead>
            <TableHead>Type</TableHead>
            <TableHead>Status</TableHead>
            <TableHead />
          </TableRow>
        </TableHeader>
        <TableBody>
          {streams.data?.streams.map((stream) => (
            <TableRow key={stream.id}>
              <TableCell className="font-medium">{stream.name}</TableCell>
              <TableCell className="font-mono text-xs text-muted-foreground">
                {stream.permalink}
              </TableCell>
              <TableCell>
                <Badge variant="outline">{stream.stream_type}</Badge>
              </TableCell>
              <TableCell>
                {stream.archived ? <Badge variant="secondary">archived</Badge> : <Badge>active</Badge>}
              </TableCell>
              <TableCell className="text-right">
                {!stream.archived && (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={async () => {
                      try {
                        await api.streams.archive(stream.permalink)
                        invalidate()
                      } catch (err) {
                        errorToast(err, "Could not archive the stream")
                      }
                    }}
                  >
                    Archive
                  </Button>
                )}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      )}
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title="New stream"
        onSubmit={() => create.mutate()}
        busy={create.isPending}
        submitDisabled={!name.trim()}
      >
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label>Name</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Newsletter" />
          </div>
          <div className="grid gap-2">
            <Label>Type</Label>
            <Select value={type} onValueChange={setType}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="transactional">transactional</SelectItem>
                <SelectItem value="broadcast">broadcast</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>
      </FormDialog>
    </div>
  )
}

// ----------------------------------------------------------- templates

function TemplateEditor({
  api,
  template,
  onClose,
  onSaved,
}: {
  api: Api
  template: Template | null
  onClose: () => void
  onSaved: () => void
}) {
  const [name, setName] = useState(template?.name ?? "")
  const [subject, setSubject] = useState(template?.subject ?? "")
  const [htmlBody, setHtmlBody] = useState(template?.html_body ?? "")
  const [textBody, setTextBody] = useState(template?.text_body ?? "")
  const [model, setModel] = useState('{ "name": "Ada" }')
  const [preview, setPreview] = useState<string | null>(null)

  async function save() {
    try {
      if (template) {
        await api.templates.update(template.permalink, {
          name,
          subject,
          html_body: htmlBody,
          text_body: textBody,
        })
      } else {
        await api.templates.create({
          name,
          subject,
          ...(htmlBody ? { html_body: htmlBody } : {}),
          ...(textBody ? { text_body: textBody } : {}),
        })
      }
      onSaved()
      onClose()
    } catch (err) {
      errorToast(err, "Could not save the template")
    }
  }

  async function renderPreview() {
    if (!template) return
    try {
      const parsed = JSON.parse(model || "{}")
      const { rendered } = await api.templates.render(template.permalink, parsed)
      setPreview(
        `Subject: ${rendered.subject ?? "—"}\n\n${rendered.text_body ?? rendered.html_body ?? ""}`,
      )
    } catch (err) {
      errorToast(err, "Rendering failed (is the model valid JSON?)")
    }
  }

  return (
    <FormDialog
      open
      onOpenChange={(open) => !open && onClose()}
      title={template ? `Edit ${template.name}` : "New template"}
      submitLabel="Save"
      onSubmit={save}
      submitDisabled={!name.trim()}
      wide
    >
        <div className="grid max-h-[70svh] gap-4 overflow-y-auto pr-1">
          <div className="grid grid-cols-2 gap-2">
            <div className="grid gap-2">
              <Label>Name</Label>
              <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="welcome" />
            </div>
            <div className="grid gap-2">
              <Label>Subject</Label>
              <Input
                value={subject}
                onChange={(e) => setSubject(e.target.value)}
                placeholder="Hello {{ name }}"
              />
            </div>
          </div>
          <div className="grid gap-2">
            <Label>Text body</Label>
            <Textarea rows={5} value={textBody} onChange={(e) => setTextBody(e.target.value)} />
          </div>
          <div className="grid gap-2">
            <Label>HTML body</Label>
            <Textarea rows={5} value={htmlBody} onChange={(e) => setHtmlBody(e.target.value)} />
          </div>
          {template && (
            <div className="grid gap-2 rounded-md border p-3">
              <Label>Preview with model (JSON)</Label>
              <div className="flex gap-2">
                <Input
                  className="font-mono text-xs"
                  value={model}
                  onChange={(e) => setModel(e.target.value)}
                />
                <Button variant="outline" onClick={renderPreview}>
                  Render
                </Button>
              </div>
              {preview && (
                <pre className="max-h-48 overflow-auto rounded bg-muted p-2 text-xs">{preview}</pre>
              )}
            </div>
          )}
        </div>
    </FormDialog>
  )
}

/// "Copy to server…" — pushes the template to a sibling server of the same
/// organization via the management API (member role or above).
function CopyTemplateDialog({
  org,
  server,
  template,
  onClose,
}: {
  org: string
  server: string
  template: Template
  onClose: () => void
}) {
  const servers = useQuery({
    queryKey: ["servers", org],
    queryFn: () => adminApi.servers(org).list(),
  })
  const targets =
    servers.data?.servers.filter((candidate) => candidate.permalink !== server) ?? []
  const [target, setTarget] = useState("")
  const [overwrite, setOverwrite] = useState(false)
  const [busy, setBusy] = useState(false)

  async function copy() {
    setBusy(true)
    try {
      await adminApi.templates(org, server).copyTo(template.permalink, target, overwrite)
      toast.success(`Copied "${template.name}" to ${target}`)
      onClose()
    } catch (err) {
      if (err instanceof ApiError && err.status === 422 && !overwrite) {
        toast.error(`${err.message} — enable "Overwrite" to replace it.`)
      } else {
        errorToast(err, "Could not copy the template")
      }
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Copy “{template.name}” to another server</DialogTitle>
        </DialogHeader>
        {targets.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            This organization has no other server to copy to.
          </p>
        ) : (
          <div className="grid gap-4">
            <div className="grid gap-2">
              <Label>Target server (same organization)</Label>
              <Select value={target || undefined} onValueChange={setTarget}>
                <SelectTrigger>
                  <SelectValue placeholder="Choose a server…" />
                </SelectTrigger>
                <SelectContent>
                  {targets.map((candidate) => (
                    <SelectItem key={candidate.id} value={candidate.permalink}>
                      {candidate.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <label className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                className="size-4 accent-primary"
                checked={overwrite}
                onChange={(e) => setOverwrite(e.target.checked)}
              />
              Overwrite if “{template.permalink}” already exists there
            </label>
          </div>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={copy} disabled={busy || !target}>
            Copy
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function Templates({ api, org, server }: { api: Api; org: string; server: string }) {
  const queryClient = useQueryClient()
  const templates = useQuery({ queryKey: ["sapi-templates"], queryFn: api.templates.list })
  const [editor, setEditor] = useState<{ open: boolean; template: Template | null }>({
    open: false,
    template: null,
  })
  const [copying, setCopying] = useState<Template | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["sapi-templates"] })

  return (
    <div>
      <PageHeader
        title="Templates"
        description="Mustache-style templates ({{ name }}) rendered per send."
        action={
          <Button size="sm" onClick={() => setEditor({ open: true, template: null })}>
            <PlusIcon className="size-4" /> New template
          </Button>
        }
      />
      {templates.data?.templates.length === 0 ? (
        <EmptyState
          icon={FileTextIcon}
          title="No templates yet"
          description="Write a Mustache-style template once and render it with fresh data on every send."
          action={{ label: "New template", onClick: () => setEditor({ open: true, template: null }) }}
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Permalink</TableHead>
              <TableHead>Subject</TableHead>
              <TableHead>Status</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {templates.data?.templates.map((template) => (
              <TableRow key={template.id}>
                <TableCell className="font-medium">{template.name}</TableCell>
                <TableCell className="font-mono text-xs text-muted-foreground">
                  {template.permalink}
                </TableCell>
                <TableCell className="max-w-56 truncate">{template.subject ?? "—"}</TableCell>
                <TableCell>
                  {template.archived ? (
                    <Badge variant="secondary">archived</Badge>
                  ) : (
                    <Badge>active</Badge>
                  )}
                </TableCell>
                <TableCell className="space-x-2 text-right">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setEditor({ open: true, template })}
                  >
                    Edit
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => setCopying(template)}>
                    Copy to server…
                  </Button>
                  {!template.archived && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        try {
                          await api.templates.archive(template.permalink)
                          invalidate()
                        } catch (err) {
                          errorToast(err, "Could not archive the template")
                        }
                      }}
                    >
                      Archive
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {editor.open && (
        <TemplateEditor
          api={api}
          template={editor.template}
          onClose={() => setEditor({ open: false, template: null })}
          onSaved={invalidate}
        />
      )}
      {copying && (
        <CopyTemplateDialog
          org={org}
          server={server}
          template={copying}
          onClose={() => setCopying(null)}
        />
      )}
    </div>
  )
}

// ---------------------------------------------------------------- shell

const MessagingContext = createContext<Api | null>(null)
type ApiP1 = ReturnType<typeof serverApiP1>
const MessagingP1Context = createContext<ApiP1 | null>(null)

/// The server API bound to the first active API credential; only
/// available inside <MessagingShell>.
export function useMessagingApi(): Api {
  const api = useContext(MessagingContext)
  if (!api) throw new Error("useMessagingApi must be used inside MessagingShell")
  return api
}

/// The P1 additions to the server API (tags, logs, opens/clicks, raw),
/// bound to the same credential. Only available inside <MessagingShell>.
export function useMessagingApiP1(): ApiP1 {
  const api = useContext(MessagingP1Context)
  if (!api) throw new Error("useMessagingApiP1 must be used inside MessagingShell")
  return api
}

const SUBTABS = [
  { value: "send", label: "Send" },
  { value: "messages", label: "Activity" },
  { value: "logs", label: "Logs" },
  { value: "queue", label: "Queue" },
  { value: "stats", label: "Stats" },
  { value: "streams", label: "Streams" },
  { value: "templates", label: "Templates" },
]

export function MessagingShell({
  org,
  server,
  children,
}: {
  org: string
  server: string
  children: React.ReactNode
}) {
  const router = useRouter()
  const pathname = usePathname() ?? ""
  const credentials = useQuery({
    queryKey: ["credentials", org, server],
    queryFn: () => adminApi.credentials(org, server).list(),
  })
  const apiKey = useMemo(
    () =>
      credentials.data?.credentials.find(
        (credential) => credential.type === "API" && !credential.hold,
      )?.key ?? null,
    [credentials.data],
  )
  const api = useMemo(() => (apiKey ? serverApi(apiKey) : null), [apiKey])
  const apiP1 = useMemo(() => (apiKey ? serverApiP1(apiKey) : null), [apiKey])
  const subtab = pathname.split("/messaging")[1]?.split("/")[1] || "send"

  if (credentials.isLoading) {
    return <p className="text-sm text-muted-foreground">Loading…</p>
  }
  if (!api || !apiP1) {
    return (
      <EmptyState
        icon={KeyRoundIcon}
        title="Connect an API credential"
        description="Messaging talks to the server's own API — create an API credential first, then come back here."
        action={{
          label: "Create API credential",
          href: `/orgs/${org}/servers/${server}/credentials`,
        }}
      />
    )
  }
  return (
    <div>
      <Tabs
        value={subtab}
        onValueChange={(value) =>
          router.push(`/orgs/${org}/servers/${server}/messaging${value === "send" ? "" : `/${value}`}`)
        }
      >
        <TabsList className="mb-4">
          {SUBTABS.map((t) => (
            <TabsTrigger key={t.value} value={t.value}>
              {t.label}
            </TabsTrigger>
          ))}
        </TabsList>
      </Tabs>
      <MessagingContext.Provider value={api}>
        <MessagingP1Context.Provider value={apiP1}>{children}</MessagingP1Context.Provider>
      </MessagingContext.Provider>
    </div>
  )
}
