"use client"

// Messaging: everything that talks to the per-server API
// (`X-Server-API-Key`). The key is picked from the server's API
// credentials — no credential, no messaging.

import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ComponentProps,
  type ReactNode,
} from "react"
import Link from "next/link"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import { useRouter } from "next/navigation"
import {
  ChevronRightIcon,
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
  SendIcon,
  Share2Icon,
  SparklesIcon,
  TriangleAlertIcon,
} from "lucide-react"
import { toast } from "sonner"
import { CopyButton, formatDate, PageHeader } from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { FormDialog } from "@/components/form-dialog"
import { MessagePill } from "@/components/status-pill"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent } from "@/components/ui/card"
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
import { DataTable } from "@/components/ui/data-table"
import { Page } from "@/components/page"
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { Textarea } from "@/components/ui/textarea"
import { Switch } from "@/components/ui/switch"
import { cn } from "@/lib/utils"
import {
  adminApi,
  ApiError,
  serverApi,
  type InsightCheck,
  type Layout,
  type Message,
  type Stream,
  type Template,
} from "@/lib/api"
import {
  httpStatusPillClass,
  parseRawEmail,
  relativeTime,
  serverApiP1,
  type ApiRequestEntry,
  type ParsedEmail,
} from "@/lib/api-p1"
import { recipientHref } from "@/lib/api-p2"
import {
  extractVariables,
  renderMustache,
  sampleModel,
  sampleValue,
  TEMPLATE_LIBRARY,
  type LibraryTemplate,
} from "@/lib/api-p3"
import { StatusPill } from "@/components/status-pill"
import { useOrgParams } from "@/lib/params"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

/// A lifecycle event pill using the shared color semantics (no
/// status-pill.tsx component — classes on the existing Badge).
type Api = ReturnType<typeof serverApi>

// ---------------------------------------------------------------- send

// The Messaging landing page: the activity list, with a "Send a message"
// CTA (top-right) that opens the send form in a dialog.
export function MessagingHome({ api }: { api: Api }) {
  const { org, server } = useOrgParams()
  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Messaging"
          description="Every message this server has sent and received."
          action={<SendMessageButton org={org} server={server} />}
          className="mb-0"
        />
      }
    >
      <Messages api={api} />
    </Page>
  )
}

// A self-contained "Send a message" launcher: the button plus the send
// form in a dialog — the same lightbox on every server screen. It brings
// its own messaging API context, so it drops in anywhere (the Messaging
// page, an empty state, a recipient view) given just org + server.
export function SendMessageButton({
  org,
  server,
  variant,
  size = "sm",
  className,
  label = "Send a message",
  defaultTo,
}: {
  org: string
  server: string
  variant?: ComponentProps<typeof Button>["variant"]
  size?: ComponentProps<typeof Button>["size"]
  className?: string
  label?: ReactNode
  defaultTo?: string
}) {
  const [open, setOpen] = useState(false)
  return (
    <>
      <Button
        variant={variant}
        size={size}
        className={className}
        onClick={() => setOpen(true)}
      >
        <SendIcon className="size-4" /> {label}
      </Button>
      <SendMessageDialog
        org={org}
        server={server}
        open={open}
        onOpenChange={setOpen}
        defaultTo={defaultTo}
      />
    </>
  )
}

// The send form in a dialog, controlled by the caller. Self-contained:
// wraps the form in its own messaging API context so it needs no wiring.
// `defaultTo` pre-fills the recipient (e.g. opened from a recipient view).
export function SendMessageDialog({
  org,
  server,
  open,
  onOpenChange,
  defaultTo,
}: {
  org: string
  server: string
  open: boolean
  onOpenChange: (open: boolean) => void
  defaultTo?: string
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Send a message</DialogTitle>
        </DialogHeader>
        <MessagingApiProvider org={org} server={server}>
          <SendBound
            org={org}
            server={server}
            defaultTo={defaultTo}
            onSent={() => onOpenChange(false)}
          />
        </MessagingApiProvider>
      </DialogContent>
    </Dialog>
  )
}

function SendBound({
  org,
  server,
  defaultTo,
  onSent,
}: {
  org: string
  server: string
  defaultTo?: string
  onSent?: () => void
}) {
  const api = useMessagingApi()
  return <Send api={api} org={org} server={server} defaultTo={defaultTo} onSent={onSent} />
}

// A message has settled once it is delivered, held, or has failed —
// until then the delivery flow keeps polling.
function messageTerminal(message: Message | undefined): boolean {
  if (!message) return false
  if (message.held) return true
  if (message.status === "Sent") return true
  return (
    message.status === "Bounced" || message.status === "HardFail" || message.bounce === true
  )
}

// The post-send view inside the dialog: instead of closing, we show what
// happened to the message — the recipients it went to, and each delivery
// attempt as the worker processes it, including the failure reason if the
// receiving server rejected it. Polls until the message settles.
function SentFlow({
  api,
  org,
  server,
  messageId,
  recipients,
  onReset,
  onClose,
}: {
  api: Api
  org: string
  server: string
  messageId: number
  recipients: string[]
  onReset: () => void
  onClose?: () => void
}) {
  const messageQuery = useQuery({
    queryKey: ["sapi-message", messageId],
    queryFn: () => api.message(messageId),
    refetchInterval: (query) =>
      messageTerminal(query.state.data?.message) ? false : 2500,
  })
  const message = messageQuery.data?.message
  const isDelivered = !!message && message.status === "Sent" && !message.held
  const isFailed =
    !!message &&
    (message.status === "Bounced" || message.status === "HardFail" || message.bounce === true)
  const isHeld = message?.held === true
  const isTerminal = isDelivered || isFailed || isHeld

  const deliveriesQuery = useQuery({
    queryKey: ["sapi-deliveries", messageId],
    queryFn: () => api.deliveries(messageId),
    refetchInterval: isTerminal ? false : 2500,
  })
  const deliveries = deliveriesQuery.data?.deliveries ?? []

  const tone = isFailed ? "failed" : isDelivered ? "ok" : isHeld ? "held" : "pending"
  const StatusIcon = tone === "ok" ? CircleCheckIcon : tone === "pending" ? RefreshCwIcon : TriangleAlertIcon
  const iconClass =
    tone === "ok"
      ? "text-emerald-600"
      : tone === "failed"
        ? "text-red-600"
        : tone === "held"
          ? "text-amber-600"
          : "text-muted-foreground animate-spin"
  const headline =
    tone === "ok"
      ? "Message delivered"
      : tone === "failed"
        ? "Delivery failed"
        : tone === "held"
          ? "Held for review"
          : "Message queued"
  const subline =
    tone === "ok"
      ? "The receiving server accepted the message."
      : tone === "failed"
        ? "The receiving server rejected the message — see the attempt below."
        : tone === "held"
          ? "The message was held and needs review before it can be delivered."
          : "Queued for delivery. This updates automatically as it goes out."

  return (
    <div className="grid gap-4">
      <div className="flex items-start gap-3 rounded-lg border p-4">
        <StatusIcon className={cn("mt-0.5 size-5 shrink-0", iconClass)} />
        <div className="min-w-0">
          <p className="font-medium">{headline}</p>
          <p className="text-sm text-muted-foreground">{subline}</p>
        </div>
      </div>

      {recipients.length > 0 && (
        <div className="grid gap-2">
          <Label>Recipients</Label>
          <ul className="grid gap-1.5">
            {recipients.map((address) => (
              <li key={address} className="flex items-center justify-between gap-2 text-sm">
                <span className="truncate">{address}</span>
                {message ? (
                  <MessagePill message={message} />
                ) : (
                  <Badge variant="outline">Queued</Badge>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      <div className="grid gap-2">
        <div className="flex items-center gap-2">
          <Label>Delivery attempts</Label>
          {!isTerminal && (
            <RefreshCwIcon className="size-3.5 animate-spin text-muted-foreground" />
          )}
        </div>
        {deliveries.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {isTerminal
              ? "No delivery attempts were recorded."
              : "Waiting for the delivery worker to pick this up…"}
          </p>
        ) : (
          <div className="grid gap-2">
            {deliveries.map((delivery) => (
              <div key={delivery.id} className="rounded-md border p-2 text-xs">
                <div className="flex flex-wrap items-center gap-2">
                  <MessagePill message={{ status: delivery.status, held: false }} />
                  <span className="text-muted-foreground">{formatDate(delivery.timestamp)}</span>
                  {delivery.sent_with_ssl && <Badge variant="outline">TLS</Badge>}
                </div>
                {delivery.details && <p className="mt-1.5">{delivery.details}</p>}
                {delivery.output && (
                  <pre className="mt-1.5 overflow-x-auto rounded bg-muted p-2 font-mono">
                    {delivery.output}
                  </pre>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="flex items-center gap-2">
        <Button variant="outline" onClick={onReset}>
          <SendIcon className="size-4" /> Send another
        </Button>
        <Button asChild variant="ghost">
          <Link href={`/orgs/${org}/servers/${server}/messaging/${messageId}`}>View details</Link>
        </Button>
        {onClose && (
          <Button className="ml-auto" onClick={onClose}>
            Done
          </Button>
        )}
      </div>
    </div>
  )
}

export function Send({
  api,
  org,
  server,
  defaultTo = "",
  onSent,
}: {
  api: Api
  org: string
  server: string
  defaultTo?: string
  onSent?: () => void
}) {
  // You can only send from a verified identity: a confirmed sender
  // address, or any local part on a verified domain.
  const senders = useQuery({
    queryKey: ["senders", org, server],
    queryFn: () => adminApi.senderAddresses(org, server).list(),
  })
  const domainsQuery = useQuery({
    queryKey: ["domains", org, server],
    queryFn: () => adminApi.domains(org, server).list(),
  })
  const confirmedSenders = (senders.data?.sender_addresses ?? []).filter(
    (s) => s.verified || s.status === "confirmed",
  )
  const verifiedDomains = (domainsQuery.data?.domains ?? []).filter((d) => d.verified)

  // The From picker holds either `addr:<email>` (a ready-made sender
  // address) or `domain:<name>` (compose a custom local part on a
  // verified domain); `from` is derived from the two.
  const [fromChoice, setFromChoice] = useState("")
  const [fromLocal, setFromLocal] = useState("")
  const fromDomain = fromChoice.startsWith("domain:") ? fromChoice.slice("domain:".length) : null
  const from = fromChoice.startsWith("addr:")
    ? fromChoice.slice("addr:".length)
    : fromDomain && fromLocal.trim()
      ? `${fromLocal.trim()}@${fromDomain}`
      : ""

  const [to, setTo] = useState(defaultTo)
  const [subject, setSubject] = useState("")
  const [textBody, setTextBody] = useState("")
  const [htmlBody, setHtmlBody] = useState("")
  const [htmlMode, setHtmlMode] = useState(false)

  const [templatePermalink, setTemplatePermalink] = useState("none")
  const templates = useQuery({ queryKey: ["sapi-templates"], queryFn: api.templates.list })
  const activeTemplate = templates.data?.templates.find((t) => t.permalink === templatePermalink)
  const templateVars = useMemo(
    () =>
      activeTemplate
        ? extractVariables(
            activeTemplate.subject,
            activeTemplate.html_body,
            activeTemplate.text_body,
          )
        : [],
    [activeTemplate],
  )

  // The template model, filled through one form field per variable — or
  // as raw JSON in expert mode.
  const [modelFields, setModelFields] = useState<Record<string, string>>({})
  const [expertModel, setExpertModel] = useState(false)
  const [modelJson, setModelJson] = useState("{}")

  // Start from a clean model whenever the selected template changes.
  useEffect(() => {
    setModelFields({})
    setExpertModel(false)
    setModelJson("{}")
  }, [templatePermalink])

  // Toggling expert mode carries the values across: fields → JSON on the
  // way in, best-effort JSON → fields on the way out.
  function toggleExpertModel(on: boolean) {
    if (on) {
      setModelJson(JSON.stringify(modelFields, null, 2))
    } else {
      try {
        const parsed = JSON.parse(modelJson || "{}")
        if (parsed && typeof parsed === "object") {
          setModelFields(
            Object.fromEntries(
              Object.entries(parsed as Record<string, unknown>).map(([k, v]) => [
                k,
                typeof v === "string" ? v : JSON.stringify(v),
              ]),
            ),
          )
        }
      } catch {
        // Unparseable JSON — keep the fields as they were.
      }
    }
    setExpertModel(on)
  }

  // After a successful send we keep the dialog open and switch to the
  // delivery flow (below) instead of closing.
  const queryClient = useQueryClient()
  const [sent, setSent] = useState<{ messageId: number; recipients: string[] } | null>(null)

  const send = useMutation({
    mutationFn: async () => {
      const recipients = to.split(",").map((address) => address.trim()).filter(Boolean)
      if (templatePermalink !== "none") {
        let model: unknown = modelFields
        if (expertModel) {
          try {
            model = JSON.parse(modelJson || "{}")
          } catch {
            throw new ApiError("ValidationError", "The template model is not valid JSON", 422)
          }
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
        ...(htmlMode && htmlBody ? { html_body: htmlBody } : {}),
      })
    },
    onSuccess: (data) => {
      const messageId = (data as { message_id: number }).message_id
      const recipients = to.split(",").map((address) => address.trim()).filter(Boolean)
      setSent({ messageId, recipients })
      toast.success("Message queued")
      // The activity list, recipient history and stats behind the dialog
      // are now stale — refresh them so the new message shows up.
      queryClient.invalidateQueries({ queryKey: ["sapi-messages"] })
      queryClient.invalidateQueries({ queryKey: ["recipient-messages"] })
      queryClient.invalidateQueries({ queryKey: ["sapi-stats"] })
    },
    onError: (err) => errorToast(err, "Sending failed"),
  })

  const identityLoading = senders.isLoading || domainsQuery.isLoading
  const hasIdentity = confirmedSenders.length > 0 || verifiedDomains.length > 0
  const canSend = from.includes("@") && to.includes("@")

  // Sent: hand off to the live delivery flow.
  if (sent) {
    return (
      <SentFlow
        api={api}
        org={org}
        server={server}
        messageId={sent.messageId}
        recipients={sent.recipients}
        onClose={onSent}
        onReset={() => {
          setSent(null)
          setTo("")
          setSubject("")
          setTextBody("")
          setHtmlBody("")
          setHtmlMode(false)
          setTemplatePermalink("none")
        }}
      />
    )
  }

  return (
    <div className="grid gap-4">
      {!hasIdentity && !identityLoading && (
        <p className="rounded-md border border-dashed p-3 text-sm text-muted-foreground">
          You need a verified domain or sender address before you can send.{" "}
          <Link
            href={`/orgs/${org}/servers/${server}/domains`}
            className="font-medium text-foreground underline underline-offset-2"
          >
            Verify a domain
          </Link>
          .
        </p>
      )}

      <div className="grid gap-2">
        <Label>From</Label>
        <Select
          value={fromChoice}
          onValueChange={(value) => {
            setFromChoice(value)
            setFromLocal("")
          }}
        >
          <SelectTrigger>
            <SelectValue placeholder="Choose a verified address" />
          </SelectTrigger>
          <SelectContent>
            {confirmedSenders.map((sender) => (
              <SelectItem key={`addr:${sender.email_address}`} value={`addr:${sender.email_address}`}>
                {sender.email_address}
              </SelectItem>
            ))}
            {verifiedDomains.map((domain) => (
              <SelectItem key={`domain:${domain.name}`} value={`domain:${domain.name}`}>
                Custom address on @{domain.name}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        {fromDomain && (
          <div className="flex items-center gap-2">
            <Input
              value={fromLocal}
              onChange={(e) => setFromLocal(e.target.value)}
              placeholder="hello"
              className="max-w-[12rem]"
            />
            <span className="text-sm text-muted-foreground">@{fromDomain}</span>
          </div>
        )}
      </div>

      <div className="grid gap-2">
        <Label>To</Label>
        <Input
          value={to}
          onChange={(e) => setTo(e.target.value)}
          placeholder="recipient@example.com, another@example.com"
        />
        <p className="text-xs text-muted-foreground">Separate multiple recipients with commas.</p>
      </div>

      <div className="grid gap-2">
        <Label>Template</Label>
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
            <Textarea rows={6} value={textBody} onChange={(e) => setTextBody(e.target.value)} />
          </div>
          <div className="flex items-center justify-between rounded-md border p-3">
            <div className="grid gap-0.5">
              <Label htmlFor="html-mode">Send HTML email (expert mode)</Label>
              <p className="text-xs text-muted-foreground">
                Add a hand-written HTML body alongside the plain text.
              </p>
            </div>
            <Switch id="html-mode" checked={htmlMode} onCheckedChange={setHtmlMode} />
          </div>
          {htmlMode && (
            <div className="grid gap-2">
              <Label>HTML body</Label>
              <Textarea
                rows={8}
                value={htmlBody}
                onChange={(e) => setHtmlBody(e.target.value)}
                className="font-mono text-xs"
              />
            </div>
          )}
        </>
      ) : (
        <div className="grid gap-3">
          <div className="flex items-center justify-between">
            <Label>Template variables</Label>
            <label className="flex items-center gap-2 text-xs text-muted-foreground">
              Edit as JSON (expert mode)
              <Switch checked={expertModel} onCheckedChange={toggleExpertModel} />
            </label>
          </div>
          {expertModel ? (
            <Textarea
              rows={8}
              value={modelJson}
              onChange={(e) => setModelJson(e.target.value)}
              className="font-mono text-xs"
            />
          ) : templateVars.length === 0 ? (
            <p className="text-sm text-muted-foreground">
              This template has no variables — nothing to fill in.
            </p>
          ) : (
            <div className="grid gap-3">
              {templateVars.map((name) => (
                <div key={name} className="grid gap-1.5">
                  <Label className="font-mono text-xs">{`{{ ${name} }}`}</Label>
                  <Input
                    value={modelFields[name] ?? ""}
                    onChange={(e) =>
                      setModelFields((prev) => ({ ...prev, [name]: e.target.value }))
                    }
                    placeholder={sampleValue(name)}
                  />
                </div>
              ))}
            </div>
          )}
        </div>
      )}

      <Button
        className="justify-self-start"
        onClick={() => send.mutate()}
        disabled={send.isPending || !canSend}
      >
        {send.isPending ? "Sending…" : "Send message"}
      </Button>
    </div>
  )
}

// ------------------------------------------------------------ messages

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
          Anyone with the link can view this message, including its content and
          delivery timeline, until the link expires. No account is needed.
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

/// One deliverability check as an expandable row (title always shown, the
/// coaching detail behind a disclosure). Warnings default to open so the
/// actionable advice is visible without a click.
function InsightRow({ check }: { check: InsightCheck }) {
  const warn = check.status === "warning"
  const [open, setOpen] = useState(warn)
  return (
    <div
      className={`rounded-md border ${
        warn
          ? "border-amber-600/30 bg-amber-600/5 dark:border-amber-400/30 dark:bg-amber-400/5"
          : "border-green-600/20 bg-green-600/5 dark:border-green-400/20 dark:bg-green-400/5"
      }`}
    >
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 p-2 text-left"
      >
        {warn ? (
          <TriangleAlertIcon className="size-4 shrink-0 text-amber-600 dark:text-amber-400" />
        ) : (
          <CircleCheckIcon className="size-4 shrink-0 text-green-600 dark:text-green-400" />
        )}
        <span className="min-w-0 flex-1 truncate text-sm font-medium">{check.title}</span>
        <ChevronRightIcon
          className={`size-4 shrink-0 text-muted-foreground transition-transform ${
            open ? "rotate-90" : ""
          }`}
        />
      </button>
      {open && (
        <p className="px-2 pb-2 pl-8 text-xs text-muted-foreground">{check.detail}</p>
      )}
    </div>
  )
}

/// The "Insights" tab: a deliverability coach. Two sections — "Doing
/// great" (ok) and "Improve" (warning) — over the server-side checks,
/// each with a section counter, plus the "Report generated on …" footer.
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

  if (data.checks.length === 0) {
    return (
      <EmptyState
        icon={CircleCheckIcon}
        title="Nothing to analyze yet"
        description="Deliverability insights appear once this message has been processed."
      />
    )
  }

  return (
    <div className="grid gap-4">
      {improve.length === 0 && (
        <div className="flex items-center gap-2 rounded-md border border-green-600/30 bg-green-600/10 p-3 text-sm text-green-700 dark:border-green-400/30 dark:bg-green-400/10 dark:text-green-400">
          <CircleCheckIcon className="size-4 shrink-0" />
          No issues found. This message follows deliverability best practices.
        </div>
      )}
      {improve.length > 0 && (
        <div className="grid gap-2">
          <h3 className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wide text-amber-600 dark:text-amber-400">
            Improve
            <Badge variant="destructive" className="px-1.5">
              {improve.length}
            </Badge>
          </h3>
          {improve.map((check) => (
            <InsightRow key={check.id} check={check} />
          ))}
        </div>
      )}
      {great.length > 0 && (
        <div className="grid gap-2">
          <h3 className="flex items-center gap-1.5 text-xs font-semibold uppercase tracking-wide text-green-700 dark:text-green-400">
            Doing great
            <Badge variant="secondary" className="px-1.5">
              {great.length}
            </Badge>
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

/// The rich message-detail content — metadata grid, lifecycle timeline
/// and the Preview / Plain Text / HTML / Raw / Insights tabs. It owns all
/// the per-message queries and reads everything from the message id, so it
/// drops straight into the detail page below.
function MessageDetailBody({ api, id }: { api: Api; id: number }) {
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

  if (!m) {
    return (
      <p className="text-sm text-muted-foreground">
        {message.isLoading ? "Loading…" : "This message could not be loaded."}
      </p>
    )
  }

  return (
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

        <div>
          <TabsContent value="preview">
            {privacyMode ? (
              <PrivacyNote />
            ) : html ? (
              <iframe
                title="Message preview"
                sandbox=""
                srcDoc={html}
                className="h-[60svh] w-full rounded-md border bg-white"
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
                <pre className="overflow-x-hidden rounded-md bg-muted p-3 pr-10 text-xs whitespace-pre-wrap break-all">
                  {html}
                </pre>
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
  )
}

/// The message detail as a full page (its own route): a back link to the
/// activity list, a header (subject as title, sender / recipient / date as
/// the subline, plus the Share action), then the rich detail body.
export function MessageDetailPage({
  api,
  org,
  server,
  id,
}: {
  api: Api
  org: string
  server: string
  id: number
}) {
  // Same query key as the body — react-query dedupes the fetch; here it
  // only feeds the header (subject, addresses, status, date).
  const message = useQuery({ queryKey: ["sapi-message", id], queryFn: () => api.message(id) })
  const [sharing, setSharing] = useState(false)
  const m = message.data?.message

  return (
    <Page
      header={
        <PageHeader
          className="mb-0 items-start"
          backHref={`/orgs/${org}/servers/${server}/messaging`}
          backLabel="Messages"
          title={m?.subject || `Message #${id}`}
          description={
            <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
              {m && <MessagePill message={m} />}
              <span>From {m?.mail_from ?? "…"}</span>
              <span>To {m?.rcpt_to ?? "…"}</span>
              {m && <span>{formatDate(m.created_at)}</span>}
            </span>
          }
          action={
            <Button variant="outline" size="sm" onClick={() => setSharing(true)}>
              <Share2Icon className="size-4" /> Share email
            </Button>
          }
        />
      }
    >
      <MessageDetailBody api={api} id={id} />

      {sharing && <ShareDialog api={api} id={id} onClose={() => setSharing(false)} />}
    </Page>
  )
}

function PrivacyNote() {
  return (
    <p className="text-sm text-muted-foreground">
      This server runs in privacy mode. Message content is not retained, so there is nothing
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
  const { org, server } = useOrgParams()
  const [scope, setScope] = useState("all")
  const [query, setQuery] = useState("")
  const [status, setStatus] = useState("all")
  const [tag, setTag] = useState("all")
  const [stream, setStream] = useState("all")
  const [range, setRange] = useState("all")

  // Opening a message now navigates to its own detail page (not a dialog).
  const messageHref = (id: number) => `/orgs/${org}/servers/${server}/messaging/${id}`

  const tags = useQuery({ queryKey: ["p1-tags"], queryFn: p1.tags })
  const streams = useQuery({ queryKey: ["sapi-streams"], queryFn: api.streams.list })

  const params = useMemo(() => {
    const q = new URLSearchParams({ per_page: "50" })
    // Omitting scope returns both directions — that's "All".
    if (scope !== "all") q.set("scope", scope)
    if (query) q.set("query", query)
    if (status !== "all") q.set("status", status)
    if (tag !== "all") q.set("tag", tag)
    if (stream !== "all") q.set("stream", stream)
    return `?${q.toString()}`
  }, [scope, query, status, tag, stream])

  const messages = useQuery({
    queryKey: ["sapi-messages", params],
    queryFn: () => api.messages(params),
    // Keep the activity list current on its own — no manual reload.
    refetchInterval: 15_000,
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

  const columns: ColumnDef<Message>[] = [
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
        // recipient history, not the message — hence stopPropagation
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
      id: "tag",
      header: "Tag",
      accessorFn: (m) => m.tag ?? "",
      cell: ({ row }) =>
        row.original.tag ? (
          <Badge variant="secondary" className="font-normal">
            {row.original.tag}
          </Badge>
        ) : (
          <span className="text-muted-foreground">—</span>
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
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="mb-4 flex shrink-0 flex-wrap items-center gap-2">
        <div className="relative w-full md:w-1/3">
          <SearchIcon className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="h-8 pl-8"
            placeholder="Search sender, subject, recipient, tag…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
        <Select value={scope} onValueChange={setScope}>
          <SelectTrigger size="sm" className="w-32">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="all">All</SelectItem>
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
          >
            <SendMessageButton org={org} server={server} />
          </EmptyState>
        )
      ) : (
        // Search + Time/Status/Tag/Stream above hit the API (server-side);
        // the DataTable's own search is a local refine over the loaded page.
        <DataTable
          columns={columns}
          data={rows}
          loading={messages.isPending}
          searchable={false}
          fillHeight
          emptyText="No events match."
          initialPageSize={20}
        />
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
  const [query, setQuery] = useState("")
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
  const filtered = query
    ? rows.filter((r) => r.path.toLowerCase().includes(query.toLowerCase()))
    : rows
  const hasFilters = method !== "all" || status !== "all"

  const columns: ColumnDef<ApiRequestEntry>[] = [
    {
      id: "method",
      header: "Method",
      accessorFn: (e) => e.method,
      cell: ({ row }) => (
        <Badge variant="outline" className="font-mono text-[10px]">
          {row.original.method}
        </Badge>
      ),
    },
    {
      id: "endpoint",
      header: "Endpoint",
      accessorFn: (e) => e.path,
      cell: ({ row }) => (
        <span className="block max-w-80 truncate font-mono text-xs transition-colors group-hover:text-primary">
          {row.original.path}
        </span>
      ),
    },
    {
      id: "status",
      header: "Status",
      accessorFn: (e) => e.status_code,
      cell: ({ row }) => <LogStatusPill code={row.original.status_code} />,
    },
    {
      id: "latency",
      header: "Latency",
      accessorFn: (e) => e.duration_ms,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className="text-muted-foreground">{row.original.duration_ms}ms</span>
      ),
    },
    {
      id: "time",
      header: "Time",
      accessorFn: (e) => e.created_at,
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

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="API request log"
          description="Every authenticated call to this server's API: method, endpoint, status and latency."
          className="mb-0"
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        <div className="mb-4 flex shrink-0 flex-wrap items-center gap-2">
          <div className="relative w-full md:w-1/3">
          <SearchIcon className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            className="h-8 pl-8"
            placeholder="Search endpoints…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
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
          <DataTable
            columns={columns}
            data={filtered}
            loading={logs.isPending}
            searchable={false}
            fillHeight
            emptyText="No requests match your search."
            initialPageSize={20}
          />
        )}
      </div>
    </Page>
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
  const rows = inbound.data?.inbound ?? []
  const statusOptions = useMemo(
    () =>
      [...new Set(rows.map((m) => m.status ?? "").filter(Boolean))].map((s) => ({
        label: s,
        value: s,
      })),
    [rows],
  )

  const columns: ColumnDef<Message>[] = [
    {
      id: "id",
      header: "#",
      accessorFn: (m) => m.id,
      meta: { align: "right" },
      cell: ({ row }) => <span className="text-muted-foreground">{row.original.id}</span>,
    },
    {
      id: "to",
      header: "To",
      accessorFn: (m) => m.rcpt_to,
      cell: ({ row }) => (
        <span className="block max-w-48 truncate">{row.original.rcpt_to}</span>
      ),
    },
    {
      id: "subject",
      header: "Subject",
      accessorFn: (m) => m.subject ?? "",
      cell: ({ row }) => (
        <span className="block max-w-64 truncate font-medium transition-colors group-hover:text-primary">
          {row.original.subject ?? "—"}
        </span>
      ),
    },
    {
      id: "status",
      header: "Status",
      accessorFn: (m) => m.status ?? "",
      filterFn: "equalsString",
      cell: ({ row }) => <MessagePill message={row.original} />,
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div className="space-x-2">
          <Button
            variant="outline"
            size="sm"
            onClick={async () => {
              try {
                await api.inboundRetry(row.original.id)
                invalidate()
                toast.success("Requeued")
              } catch (err) {
                errorToast(err, "Retry failed")
              }
            }}
          >
            Retry
          </Button>
          {row.original.held && (
            <Button
              variant="outline"
              size="sm"
              onClick={async () => {
                try {
                  await api.inboundBypass(row.original.id)
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
        </div>
      ),
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Inbound & held messages"
          description="Retry failed inbound deliveries or bypass holds."
          className="mb-0"
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {rows.length === 0 ? (
          <EmptyState
            icon={InboxIcon}
            title="Nothing waiting"
            description="Failed inbound deliveries and held messages show up here for retry or bypass."
          />
        ) : (
          <DataTable
            columns={columns}
            data={rows}
            loading={inbound.isPending}
            searchKeys={["rcpt_to", "subject"]}
            searchPlaceholder="Search held & inbound…"
            filters={
              statusOptions.length > 1
                ? [{ columnId: "status", label: "Any status", options: statusOptions }]
                : []
            }
            fillHeight
            emptyText="Nothing matches your search."
            initialPageSize={20}
          />
        )}
      </div>
    </Page>
  )
}

// --------------------------------------------------------------- stats

export function StatsView({ api }: { api: Api }) {
  const stats = useQuery({ queryKey: ["sapi-stats"], queryFn: api.stats, refetchInterval: 15_000 })
  const bounces = useQuery({ queryKey: ["sapi-bounces"], queryFn: api.bounces })
  const s = stats.data?.stats

  const bounceColumns: ColumnDef<Message>[] = [
    {
      id: "to",
      header: "To",
      accessorFn: (m) => m.rcpt_to,
      cell: ({ row }) => (
        <span className="block max-w-64 truncate font-medium transition-colors group-hover:text-primary">
          {row.original.rcpt_to}
        </span>
      ),
    },
    {
      id: "subject",
      header: "Subject",
      accessorFn: (m) => m.subject ?? "",
      cell: ({ row }) => (
        <span className="block max-w-64 truncate">{row.original.subject ?? "—"}</span>
      ),
    },
    {
      id: "created",
      header: "Created",
      accessorFn: (m) => m.created_at,
      meta: { align: "right" },
      cell: ({ row }) => (
        <span className="whitespace-nowrap text-muted-foreground">
          {formatDate(row.original.created_at)}
        </span>
      ),
    },
  ]

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
            description="Your bounce list is clean. Deliverability is looking good."
          />
        ) : (
          <DataTable
            columns={bounceColumns}
            data={bounces.data?.bounces ?? []}
            loading={bounces.isPending}
            searchKeys={["rcpt_to", "subject"]}
            searchPlaceholder="Search bounces…"
            emptyText="No bounces match your search."
            initialPageSize={20}
          />
        )}
      </div>
    </div>
  )
}

// ------------------------------------------------------------- streams

export function Streams({ api }: { api: Api }) {
  const queryClient = useQueryClient()
  const { org, server } = useOrgParams()
  const streams = useQuery({ queryKey: ["sapi-streams"], queryFn: api.streams.list })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [type, setType] = useState("transactional")
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["sapi-streams"] })

  // Opening a stream navigates to its own detail page (like the message list).
  const detailHref = (permalink: string) =>
    `/orgs/${org}/servers/${server}/streams/${encodeURIComponent(permalink)}`

  const create = useMutation({
    mutationFn: () => api.streams.create({ name, stream_type: type }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setName("")
    },
    onError: (err) => errorToast(err, "Could not create the stream"),
  })

  const columns: ColumnDef<Stream>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (s) => s.name,
      cell: ({ row }) => (
        <Link
          href={detailHref(row.original.permalink)}
          className="block max-w-64 truncate font-medium transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.name}
        </Link>
      ),
    },
    {
      id: "permalink",
      header: "Permalink",
      accessorFn: (s) => s.permalink,
      cell: ({ row }) => (
        <span className="font-mono text-xs text-muted-foreground">{row.original.permalink}</span>
      ),
    },
    {
      id: "type",
      header: "Type",
      accessorFn: (s) => s.stream_type,
      cell: ({ row }) => <Badge variant="outline">{row.original.stream_type}</Badge>,
    },
    {
      id: "status",
      header: "Status",
      enableSorting: false,
      accessorFn: (s) => (s.archived ? "archived" : "active"),
      filterFn: (row, _id, value) => (row.original.archived ? "archived" : "active") === value,
      cell: ({ row }) =>
        row.original.archived ? (
          <Badge variant="secondary">archived</Badge>
        ) : (
          <Badge>active</Badge>
        ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div className="flex items-center justify-end gap-1">
          {!row.original.archived && (
            <Button
              variant="ghost"
              size="sm"
              onClick={async () => {
                try {
                  await api.streams.archive(row.original.permalink)
                  invalidate()
                } catch (err) {
                  errorToast(err, "Could not archive the stream")
                }
              }}
            >
              Archive
            </Button>
          )}
          <Button variant="ghost" size="icon" aria-label="View stream" asChild>
            <Link href={detailHref(row.original.permalink)}>
              <ChevronRightIcon className="size-4" />
            </Link>
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
          title="Message streams"
          description="Group outgoing mail (transactional / broadcast) for stats and policies."
          action={
            <Button size="sm" onClick={() => setOpen(true)}>
              <PlusIcon className="size-4" /> New stream
            </Button>
          }
          className="mb-0"
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {streams.isSuccess && streams.data.streams.length === 0 ? (
          <EmptyState
            icon={LayersIcon}
            title="No streams yet"
            description="Streams separate transactional from broadcast mail for cleaner stats and policies."
            action={{ label: "New stream", onClick: () => setOpen(true) }}
          />
        ) : (
          <DataTable
            columns={columns}
            data={streams.data?.streams ?? []}
            loading={streams.isPending}
            searchKeys={["name", "permalink"]}
            searchPlaceholder="Search streams…"
            fillHeight
            emptyText="No streams match your search."
            initialPageSize={20}
            filters={[
              {
                columnId: "status",
                label: "Status",
                options: [
                  { label: "Active", value: "active" },
                  { label: "Archived", value: "archived" },
                ],
              },
            ]}
          />
        )}
      </div>
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
    </Page>
  )
}

/// One message stream as a full page (its own route): a back link to the
/// streams list, a header (stream name as title, type / permalink as the
/// subline, plus the archive + "view messages" actions), then a details
/// section. The messaging API has no single-get for a stream, so we read
/// the list and find by permalink — the same shape Templates/TemplateEditor
/// use for a single template.
export function StreamDetail({
  api,
  org,
  server,
  permalink,
}: {
  api: Api
  org: string
  server: string
  permalink: string
}) {
  const queryClient = useQueryClient()
  const streams = useQuery({ queryKey: ["sapi-streams"], queryFn: api.streams.list })
  const stream = streams.data?.streams.find((s) => s.permalink === permalink)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["sapi-streams"] })

  // Broadcast streams are marketing streams: surface the opt-outs suppressed
  // on this stream (the management-API suppression list carries the scope).
  const isBroadcast = stream?.stream_type === "broadcast"
  const suppressions = useQuery({
    queryKey: ["suppressions", org, server],
    queryFn: () => adminApi.suppressions(org, server).list(),
    enabled: isBroadcast,
  })
  const streamUnsubs = (suppressions.data?.suppressions ?? []).filter(
    (s) => s.stream_id === stream?.id,
  )

  // Compliance: the server's postal address goes in the broadcast footer.
  const serverRec = useQuery({
    queryKey: ["server", org, server],
    queryFn: () => adminApi.servers(org).get(server),
    enabled: isBroadcast,
  })
  const hasAddress = !!serverRec.data?.server.broadcast_physical_address

  // Opt-in subscribers (broadcast only): consent gate for marketing sends.
  const subscribers = useQuery({
    queryKey: ["sapi-subscribers", permalink],
    queryFn: () => api.streams.subscribers(permalink).list(),
    enabled: isBroadcast,
  })
  const subs = subscribers.data?.subscribers ?? []
  const [newSub, setNewSub] = useState("")
  const invalidateSubs = () =>
    queryClient.invalidateQueries({ queryKey: ["sapi-subscribers", permalink] })
  const addSub = useMutation({
    mutationFn: () => api.streams.subscribers(permalink).add(newSub.trim()),
    onSuccess: () => {
      invalidateSubs()
      setNewSub("")
    },
    onError: (err) => errorToast(err, "Could not add the subscriber"),
  })
  const removeSub = useMutation({
    mutationFn: (address: string) => api.streams.subscribers(permalink).remove(address),
    onSuccess: invalidateSubs,
    onError: (err) => errorToast(err, "Could not remove the subscriber"),
  })
  const complain = useMutation({
    mutationFn: (address: string) => api.streams.subscribers(permalink).complaint(address),
    onSuccess: () => {
      invalidateSubs()
      toast.success("Recorded as a spam complaint")
    },
    onError: (err) => errorToast(err, "Could not record the complaint"),
  })
  const [importText, setImportText] = useState("")
  const importSubs = useMutation({
    mutationFn: () =>
      api.streams
        .subscribers(permalink)
        .import(
          importText
            .split(/[\s,;]+/)
            .map((a) => a.trim())
            .filter(Boolean),
        ),
    onSuccess: (data) => {
      invalidateSubs()
      setImportText("")
      toast.success(`Added ${data.added} subscriber${data.added === 1 ? "" : "s"}`)
    },
    onError: (err) => errorToast(err, "Could not import subscribers"),
  })

  // Campaign: send the same content to every subscriber of this stream.
  const [campaignOpen, setCampaignOpen] = useState(false)
  const [campaign, setCampaign] = useState({ from: "", subject: "", html_body: "" })
  const sendCampaign = useMutation({
    mutationFn: () =>
      api.streams.campaign(permalink, {
        from: campaign.from,
        subject: campaign.subject,
        ...(campaign.html_body ? { html_body: campaign.html_body } : {}),
      }),
    onSuccess: (data) => {
      toast.success(
        `Campaign queued to ${data.queued} subscriber${data.queued === 1 ? "" : "s"}` +
          (data.skipped ? ` (${data.skipped} over the cap skipped)` : ""),
      )
      setCampaignOpen(false)
    },
    onError: (err) => errorToast(err, "Could not send the campaign"),
  })

  // Reputation isolation: which IP pool this stream sends from.
  const pools = useQuery({ queryKey: ["admin", "ip-pools"], queryFn: adminApi.ipPools.list })
  const setPool = useMutation({
    mutationFn: (ip_pool_id: number | null) => api.streams.update(permalink, { ip_pool_id }),
    onSuccess: () => {
      invalidate()
      toast.success("Sending IP pool updated")
    },
    onError: (err) => errorToast(err, "Could not update the IP pool"),
  })

  const archive = useMutation({
    mutationFn: () => api.streams.archive(permalink),
    onSuccess: () => {
      invalidate()
      toast.success("Stream archived")
    },
    onError: (err) => errorToast(err, "Could not archive the stream"),
  })

  const backHref = `/orgs/${org}/servers/${server}/streams`

  // Same list query as the streams list — react-query dedupes the fetch;
  // until it settles (or if the permalink is unknown) show a light state.
  if (!stream) {
    return (
      <Page
        header={
          <PageHeader
            className="mb-0"
            backHref={backHref}
            backLabel="Streams"
            title={permalink}
          />
        }
      >
        <p className="text-sm text-muted-foreground">
          {streams.isLoading ? "Loading…" : "This stream could not be found."}
        </p>
      </Page>
    )
  }

  return (
    <Page
      header={
        <PageHeader
          className="mb-0 items-start"
          backHref={backHref}
          backLabel="Streams"
          title={stream.name}
          description={`${stream.stream_type} · ${stream.permalink}`}
          action={
            <div className="flex flex-wrap items-center gap-2">
              <Button variant="outline" size="sm" asChild>
                <Link
                  href={`/orgs/${org}/servers/${server}/messaging?stream=${encodeURIComponent(
                    stream.permalink,
                  )}`}
                >
                  <MailIcon className="size-4" /> View messages in this stream
                </Link>
              </Button>
              {!stream.archived && (
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => archive.mutate()}
                  disabled={archive.isPending}
                >
                  {archive.isPending ? "Archiving…" : "Archive"}
                </Button>
              )}
            </div>
          }
        />
      }
    >
      <div className="space-y-4">
        <div className="rounded-md border p-4">
          <div className="grid grid-cols-[6rem_1fr] gap-x-3 gap-y-2 text-sm">
            <MetaRow label="Name" value={stream.name} />
            <MetaRow label="Type" value={stream.stream_type} />
            <MetaRow label="Permalink" value={stream.permalink} copy />
            <MetaRow label="Status" value={stream.archived ? "Archived" : "Active"} />
          </div>
        </div>

        <div className="rounded-md border p-4">
          <h2 className="text-base font-semibold">Sending</h2>
          <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
            The IP pool this stream sends from. Broadcast streams should use a separate pool to
            keep marketing reputation isolated from transactional mail.
          </p>
          <div className="mt-3 grid max-w-sm gap-2">
            <Label>IP pool</Label>
            <Select
              value={stream.ip_pool_id?.toString() ?? "none"}
              onValueChange={(v) => setPool.mutate(v === "none" ? null : Number(v))}
              disabled={setPool.isPending}
            >
              <SelectTrigger className="w-full">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">Server default pool</SelectItem>
                {pools.data?.ip_pools.map((p) => (
                  <SelectItem key={p.id} value={p.id.toString()}>
                    {p.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        {isBroadcast && (
          <>
          <div className="rounded-md border p-4">
            <h2 className="text-base font-semibold">Marketing stream</h2>
            <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
              Messages on this stream carry a one-click List-Unsubscribe header and a CAN-SPAM
              footer (unsubscribe link + postal address). Recipients who unsubscribe are
              suppressed on this stream only, so opt-outs here never block your transactional mail.
            </p>
            {!hasAddress && (
              <p className="mt-3 rounded-md border border-amber-300/60 bg-amber-50 p-3 text-sm text-amber-900">
                No broadcast postal address is set — required for CAN-SPAM.{" "}
                <Link
                  href={`/orgs/${org}/servers/${server}/settings`}
                  className="font-medium underline underline-offset-2"
                >
                  Add one in Settings
                </Link>
                .
              </p>
            )}
            <div className="mt-4 flex flex-wrap items-center gap-4">
              <div className="mr-2">
                <div className="text-2xl font-semibold tabular-nums">{streamUnsubs.length}</div>
                <div className="text-xs text-muted-foreground">unsubscribed / suppressed</div>
              </div>
              <Button size="sm" onClick={() => setCampaignOpen(true)}>
                <SendIcon className="size-4" /> Send campaign
              </Button>
              <Button variant="outline" size="sm" asChild>
                <Link href={`/orgs/${org}/servers/${server}/suppressions`}>
                  View suppressions
                </Link>
              </Button>
            </div>
          </div>

          <div className="rounded-md border p-4">
            <h2 className="text-base font-semibold">Subscribers</h2>
            <p className="mt-1 max-w-2xl text-sm text-muted-foreground">
              Broadcast messages may only be sent to opted-in subscribers. Add addresses here;
              an unsubscribe removes consent automatically.
            </p>
            <div className="mt-3 flex flex-wrap gap-2">
              <Input
                value={newSub}
                onChange={(e) => setNewSub(e.target.value)}
                placeholder="subscriber@example.com"
                className="h-8 max-w-xs"
              />
              <Button
                size="sm"
                onClick={() => addSub.mutate()}
                disabled={!newSub.trim() || addSub.isPending}
              >
                Add subscriber
              </Button>
            </div>
            <div className="mt-3 grid gap-2">
              <Label className="text-xs text-muted-foreground">
                Import (one address per line or comma-separated)
              </Label>
              <Textarea
                rows={3}
                value={importText}
                onChange={(e) => setImportText(e.target.value)}
                placeholder={"a@example.com\nb@example.com"}
                className="font-mono text-xs"
              />
              <Button
                variant="outline"
                size="sm"
                className="justify-self-start"
                onClick={() => importSubs.mutate()}
                disabled={!importText.trim() || importSubs.isPending}
              >
                Import subscribers
              </Button>
            </div>
            <div className="mt-3">
              {subscribers.isLoading ? (
                <p className="text-sm text-muted-foreground">Loading…</p>
              ) : subs.length === 0 ? (
                <p className="text-sm text-muted-foreground">No subscribers yet.</p>
              ) : (
                <ul className="divide-y">
                  {subs.map((s) => (
                    <li
                      key={s.id}
                      className="flex items-center justify-between gap-2 py-2 text-sm"
                    >
                      <span className="min-w-0 truncate">{s.address}</span>
                      <span className="flex shrink-0 items-center gap-2">
                        <Badge variant={s.status === "subscribed" ? "secondary" : "outline"}>
                          {s.status}
                        </Badge>
                        {s.status === "subscribed" && (
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => complain.mutate(s.address)}
                            disabled={complain.isPending}
                          >
                            Mark complaint
                          </Button>
                        )}
                        <Button
                          variant="ghost"
                          size="sm"
                          onClick={() => removeSub.mutate(s.address)}
                          disabled={removeSub.isPending}
                        >
                          Remove
                        </Button>
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </div>
          </div>
          </>
        )}
      </div>

      <Dialog open={campaignOpen} onOpenChange={setCampaignOpen}>
        <DialogContent className="max-h-[85vh] overflow-y-auto sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>Send campaign</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <p className="text-sm text-muted-foreground">
              Sends this message to all {subs.filter((s) => s.status === "subscribed").length}{" "}
              subscriber(s) of this stream. Every copy carries the unsubscribe footer and header.
            </p>
            <div className="grid gap-2">
              <Label>From</Label>
              <Input
                value={campaign.from}
                onChange={(e) => setCampaign({ ...campaign, from: e.target.value })}
                placeholder="news@yourdomain.com"
              />
            </div>
            <div className="grid gap-2">
              <Label>Subject</Label>
              <Input
                value={campaign.subject}
                onChange={(e) => setCampaign({ ...campaign, subject: e.target.value })}
              />
            </div>
            <div className="grid gap-2">
              <Label>HTML body</Label>
              <Textarea
                rows={8}
                value={campaign.html_body}
                onChange={(e) => setCampaign({ ...campaign, html_body: e.target.value })}
                className="font-mono text-xs"
              />
            </div>
            <Button
              className="justify-self-start"
              onClick={() => sendCampaign.mutate()}
              disabled={
                sendCampaign.isPending || !campaign.from.includes("@") || !campaign.subject.trim()
              }
            >
              {sendCampaign.isPending
                ? "Sending…"
                : `Send to ${subs.filter((s) => s.status === "subscribed").length} subscribers`}
            </Button>
          </div>
        </DialogContent>
      </Dialog>
    </Page>
  )
}

// ----------------------------------------------------------- templates

/// A scaled, sandboxed thumbnail of an HTML body — a fixed 600px canvas
/// shrunk into the card. Variables are filled with sample values so the
/// preview reads like a rendered mail, not raw Mustache.
function TemplateThumbnail({
  html,
  subject,
  textBody,
}: {
  html: string | null
  subject?: string | null
  textBody?: string | null
}) {
  if (!html) {
    return (
      <div className="flex h-36 items-center justify-center rounded-t-md border-b bg-muted/40 text-xs text-muted-foreground">
        Plain-text template
      </div>
    )
  }
  const rendered = renderMustache(html, sampleModel(subject, html, textBody))
  return (
    <div className="h-36 overflow-hidden rounded-t-md border-b bg-white">
      <iframe
        title="Template thumbnail"
        sandbox=""
        srcDoc={rendered}
        tabIndex={-1}
        aria-hidden
        className="pointer-events-none origin-top-left"
        style={{ width: "600px", height: "480px", transform: "scale(0.44)", border: "0" }}
      />
    </div>
  )
}

/// "Start from library" — the gallery-wizard over the 20 bundled
/// templates (masterplan §4.7). Each entry previews its thumbnail; Import
/// calls the create API with the full body.
function LibraryWizard({
  api,
  existingPermalinks,
  onClose,
  onImported,
}: {
  api: Api
  existingPermalinks: Set<string>
  onClose: () => void
  onImported: () => void
}) {
  const [importing, setImporting] = useState<string | null>(null)

  async function importTemplate(template: LibraryTemplate) {
    setImporting(template.permalink)
    try {
      await api.templates.create({
        name: template.name,
        subject: template.subject,
        html_body: template.html_body,
        text_body: template.text_body,
      })
      toast.success(`Imported “${template.name}”`)
      onImported()
    } catch (err) {
      errorToast(err, "Could not import the template")
    } finally {
      setImporting(null)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <DialogTitle>Start from the library</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          Twenty production-ready transactional templates: account lifecycle, security,
          collaboration and commerce. Import one to make it your own.
        </p>
        <div className="grid max-h-[65svh] grid-cols-2 gap-3 overflow-y-auto pr-1 sm:grid-cols-3">
          {TEMPLATE_LIBRARY.map((template) => {
            const already = existingPermalinks.has(template.permalink)
            return (
              <Card key={template.permalink} className="gap-0 overflow-hidden p-0">
                <TemplateThumbnail
                  html={template.html_body}
                  subject={template.subject}
                  textBody={template.text_body}
                />
                <CardContent className="grid gap-2 p-3">
                  <div className="min-w-0">
                    <p className="truncate text-sm font-medium">{template.name}</p>
                    <p className="line-clamp-2 text-xs text-muted-foreground">
                      {template.description}
                    </p>
                  </div>
                  <Button
                    size="sm"
                    variant={already ? "outline" : "default"}
                    disabled={already || importing !== null}
                    onClick={() => importTemplate(template)}
                  >
                    {already
                      ? "Already imported"
                      : importing === template.permalink
                        ? "Importing…"
                        : "Import"}
                  </Button>
                </CardContent>
              </Card>
            )
          })}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Done
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
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
        toast.error(`${err.message}. Enable "Overwrite" to replace it.`)
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

/// The templates gallery (masterplan §4.7): rendered mini-thumbnails,
/// slug badge and Published/Archived status; "New template" and Edit open
/// the focus-mode split editor (a route), "Start from library" the
/// gallery-wizard, "Copy to server…" the sibling-server push.
/// Manage reusable layouts (the shared logo/address/social chrome that
/// wraps template bodies). A default HTML wrapper seeds a new layout so
/// the required {{{ content }}} placeholder is never forgotten.
const DEFAULT_WRAPPER = `<table role="presentation" width="100%" style="background:#f4f4f5;padding:24px 0;font-family:Arial,sans-serif">
  <tr><td align="center">
    <table role="presentation" width="560" style="background:#fff;border-radius:12px;overflow:hidden">
      <tr><td style="padding:24px 32px;font-size:18px;font-weight:700;color:#18181b">{{ product }}</td></tr>
      <tr><td style="padding:0 32px 24px">{{{ content }}}</td></tr>
      <tr><td style="padding:20px 32px;font-size:12px;color:#a1a1aa;border-top:1px solid #e4e4e7">
        Acme GmbH · Camelweg 1 · 10115 Berlin<br>
        <a href="{{ unsubscribe_url }}" style="color:#71717a">Unsubscribe</a>
      </td></tr>
    </table>
  </td></tr>
</table>`

function LayoutsDialog({ api, onClose }: { api: Api; onClose: () => void }) {
  const queryClient = useQueryClient()
  const layouts = useQuery({ queryKey: ["sapi-layouts"], queryFn: api.layouts.list })
  const [editing, setEditing] = useState<Layout | "new" | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["sapi-layouts"] })
  const rows = layouts.data?.layouts ?? []

  const columns: ColumnDef<Layout>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (l) => l.name,
      cell: ({ row }) => (
        <span className="block max-w-64 truncate font-medium transition-colors group-hover:text-primary">
          {row.original.name}
        </span>
      ),
    },
    {
      id: "slug",
      header: "Slug",
      accessorFn: (l) => l.permalink,
      cell: ({ row }) => (
        <span className="font-mono text-xs text-muted-foreground">{row.original.permalink}</span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div className="space-x-2">
          <Button variant="outline" size="sm" onClick={() => setEditing(row.original)}>
            Edit
          </Button>
          <Button
            variant="ghost"
            size="sm"
            onClick={async () => {
              try {
                await api.layouts.delete(row.original.permalink)
                invalidate()
              } catch (err) {
                errorToast(err, "Could not delete the layout")
              }
            }}
          >
            Delete
          </Button>
        </div>
      ),
    },
  ]

  if (editing) {
    return (
      <LayoutEditorDialog
        api={api}
        layout={editing === "new" ? null : editing}
        onClose={() => setEditing(null)}
        onSaved={() => {
          invalidate()
          setEditing(null)
        }}
      />
    )
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Layouts</DialogTitle>
        </DialogHeader>
        <p className="text-sm text-muted-foreground">
          Reusable wrappers for the logo, postal address and social links every mail shares.
          Templates pick a layout, and it wraps their rendered body.
        </p>
        {rows.length === 0 ? (
          <div className="rounded-md border border-dashed p-6 text-center text-sm text-muted-foreground">
            No layouts yet. Create one to share branding across templates.
          </div>
        ) : (
          <DataTable
            columns={columns}
            data={rows}
            loading={layouts.isPending}
            searchKeys={["name", "permalink"]}
            searchPlaceholder="Search layouts…"
            emptyText="No layouts match your search."
            initialPageSize={10}
          />
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Close
          </Button>
          <Button onClick={() => setEditing("new")}>
            <PlusIcon className="size-4" /> New layout
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/// Create/edit one layout: an HTML wrapper (required, must embed
/// {{{ content }}}) and an optional plain-text wrapper, with a live
/// preview of a sample body wrapped by it.
function LayoutEditorDialog({
  api,
  layout,
  onClose,
  onSaved,
}: {
  api: Api
  layout: Layout | null
  onClose: () => void
  onSaved: () => void
}) {
  const [name, setName] = useState(layout?.name ?? "")
  const [htmlWrapper, setHtmlWrapper] = useState(layout?.html_wrapper ?? DEFAULT_WRAPPER)
  const [textWrapper, setTextWrapper] = useState(layout?.text_wrapper ?? "")
  const [busy, setBusy] = useState(false)

  const hasContent = /\{\{\{\s*content\s*\}\}\}|\{\{&\s*content\s*\}\}/.test(htmlWrapper)
  const preview = hasContent
    ? renderMustache(htmlWrapper, {
        product: "Acme",
        unsubscribe_url: "#",
        content: "<p style='margin:0'>Your message body appears here.</p>",
      })
    : ""

  async function save() {
    setBusy(true)
    try {
      if (layout) {
        await api.layouts.update(layout.permalink, {
          name,
          html_wrapper: htmlWrapper,
          text_wrapper: textWrapper,
        })
      } else {
        await api.layouts.create({
          name,
          html_wrapper: htmlWrapper,
          ...(textWrapper ? { text_wrapper: textWrapper } : {}),
        })
      }
      toast.success(layout ? "Layout saved" : "Layout created")
      onSaved()
    } catch (err) {
      errorToast(err, "Could not save the layout")
    } finally {
      setBusy(false)
    }
  }

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-4xl">
        <DialogHeader>
          <DialogTitle>{layout ? `Edit ${layout.name}` : "New layout"}</DialogTitle>
        </DialogHeader>
        <div className="grid gap-4 lg:grid-cols-2">
          <div className="grid content-start gap-3">
            <div className="grid gap-1.5">
              <Label>Name</Label>
              <Input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="Brand"
              />
            </div>
            <div className="grid gap-1.5">
              <Label>HTML wrapper</Label>
              <Textarea
                rows={14}
                value={htmlWrapper}
                onChange={(e) => setHtmlWrapper(e.target.value)}
                className="font-mono text-xs"
              />
              {!hasContent && (
                <p className="text-xs text-red-600 dark:text-red-400">
                  The wrapper must embed the body with {"{{{ content }}}"} (raw interpolation).
                </p>
              )}
            </div>
            <div className="grid gap-1.5">
              <Label>Plain-text wrapper (optional)</Label>
              <Textarea
                rows={4}
                value={textWrapper}
                onChange={(e) => setTextWrapper(e.target.value)}
                className="font-mono text-xs"
                placeholder={"{{& content }}\n--\nAcme GmbH"}
              />
            </div>
          </div>
          <div className="grid content-start gap-1.5">
            <Label>Preview</Label>
            {preview ? (
              <iframe
                title="Layout preview"
                sandbox=""
                srcDoc={preview}
                className="h-[60svh] w-full rounded-md border bg-white"
              />
            ) : (
              <p className="rounded-md border border-dashed p-8 text-center text-sm text-muted-foreground">
                Add {"{{{ content }}}"} to see the preview.
              </p>
            )}
          </div>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button onClick={save} disabled={busy || !name.trim() || !hasContent}>
            {busy ? "Saving…" : layout ? "Save" : "Create"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function Templates({ api, org, server }: { api: Api; org: string; server: string }) {
  const router = useRouter()
  const queryClient = useQueryClient()
  const templates = useQuery({ queryKey: ["sapi-templates"], queryFn: api.templates.list })
  const [library, setLibrary] = useState(false)
  const [layoutsOpen, setLayoutsOpen] = useState(false)
  const [copying, setCopying] = useState<Template | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["sapi-templates"] })

  const base = `/orgs/${org}/servers/${server}/templates`
  const editorHref = (permalink: string) => `${base}/${encodeURIComponent(permalink)}`
  const existingPermalinks = new Set(templates.data?.templates.map((t) => t.permalink) ?? [])

  return (
    <Page
      variant="scroll"
      header={
        <PageHeader
          title="Templates"
          description="Mustache-style templates ({{ name }}) rendered per send."
          action={
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => setLayoutsOpen(true)}>
                <LayersIcon className="size-4" /> Layouts
              </Button>
              <Button variant="outline" size="sm" onClick={() => setLibrary(true)}>
                <SparklesIcon className="size-4" /> Start from library
              </Button>
              <Button size="sm" onClick={() => router.push(`${base}/new`)}>
                <PlusIcon className="size-4" /> New template
              </Button>
            </div>
          }
          className="mb-0"
        />
      }
    >
      {templates.data?.templates.length === 0 ? (
        <EmptyState
          icon={FileTextIcon}
          title="No templates yet"
          description="Write a Mustache-style template once and render it with fresh data on every send, or start from one of 20 ready-made designs."
          action={{ label: "New template", onClick: () => router.push(`${base}/new`) }}
          secondaryAction={{ label: "Start from library", onClick: () => setLibrary(true) }}
        />
      ) : (
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {templates.data?.templates.map((template) => (
            <Card key={template.id} className="gap-0 overflow-hidden p-0">
              <TemplateThumbnail
                html={template.html_body}
                subject={template.subject}
                textBody={template.text_body}
              />
              <CardContent className="grid gap-2 p-3">
                <div className="flex items-start justify-between gap-2">
                  <div className="min-w-0">
                    <Link
                      href={editorHref(template.permalink)}
                      className="block truncate text-sm font-medium transition-colors hover:text-primary hover:underline"
                    >
                      {template.name}
                    </Link>
                    <p className="truncate font-mono text-xs text-muted-foreground">
                      {template.permalink}
                    </p>
                  </div>
                  <StatusPill status={template.archived ? "draft" : "published"} />
                </div>
                <p className="truncate text-xs text-muted-foreground">{template.subject ?? "—"}</p>
                <div className="mt-1 flex flex-wrap items-center gap-2">
                  <Button variant="outline" size="sm" asChild>
                    <Link href={editorHref(template.permalink)}>Edit</Link>
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
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}
      {library && (
        <LibraryWizard
          api={api}
          existingPermalinks={existingPermalinks}
          onClose={() => setLibrary(false)}
          onImported={invalidate}
        />
      )}
      {layoutsOpen && <LayoutsDialog api={api} onClose={() => setLayoutsOpen(false)} />}
      {copying && (
        <CopyTemplateDialog
          org={org}
          server={server}
          template={copying}
          onClose={() => setCopying(null)}
        />
      )}
    </Page>
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


// Provides the server messaging API contexts (base + P1), bound to an
// active API credential — or a "connect a credential" prompt if none.
// Reusable outside the messaging tabs (e.g. the Statistics block on the
// server dashboard).
export function MessagingApiProvider({
  org,
  server,
  children,
}: {
  org: string
  server: string
  children: React.ReactNode
}) {
  const credentials = useQuery({
    queryKey: ["credentials", org, server],
    queryFn: () => adminApi.credentials(org, server).list(),
  })
  const apiKey = useMemo(
    () =>
      credentials.data?.credentials.find((c) => c.type === "API" && !c.hold)?.key ?? null,
    [credentials.data],
  )
  const api = useMemo(() => (apiKey ? serverApi(apiKey) : null), [apiKey])
  const apiP1 = useMemo(() => (apiKey ? serverApiP1(apiKey) : null), [apiKey])

  if (credentials.isLoading) {
    return <p className="text-sm text-muted-foreground">Loading…</p>
  }
  if (!api || !apiP1) {
    return (
      <EmptyState
        icon={KeyRoundIcon}
        title="Connect an API credential"
        description="Delivery stats come from the server's own API. Create an API credential to light this up."
        action={{
          label: "Create API credential",
          href: `/orgs/${org}/servers/${server}/credentials`,
        }}
      />
    )
  }
  return (
    <MessagingContext.Provider value={api}>
      <MessagingP1Context.Provider value={apiP1}>{children}</MessagingP1Context.Provider>
    </MessagingContext.Provider>
  )
}

// The messaging "Summary" (delivery counters + recent bounces), self-
// contained for use outside the messaging tabs (e.g. the server dashboard).
function StatsViewBound() {
  const api = useMessagingApi()
  return <StatsView api={api} />
}

export function ServerSummary({ org, server }: { org: string; server: string }) {
  return (
    <MessagingApiProvider org={org} server={server}>
      <StatsViewBound />
    </MessagingApiProvider>
  )
}

// Standalone (own server-nav item) wrappers for messaging resources that
// moved out of the messaging tab bar. Each binds the messaging API.
function StreamsBound() {
  const api = useMessagingApi()
  return <Streams api={api} />
}
export function ServerStreams({ org, server }: { org: string; server: string }) {
  return (
    <MessagingApiProvider org={org} server={server}>
      <StreamsBound />
    </MessagingApiProvider>
  )
}

function QueueBound() {
  const api = useMessagingApi()
  return <InboundQueue api={api} />
}
export function ServerQueue({ org, server }: { org: string; server: string }) {
  return (
    <MessagingApiProvider org={org} server={server}>
      <QueueBound />
    </MessagingApiProvider>
  )
}

export function ServerLogs({ org, server }: { org: string; server: string }) {
  return (
    <MessagingApiProvider org={org} server={server}>
      <LogsView />
    </MessagingApiProvider>
  )
}

function TemplatesBound({ org, server }: { org: string; server: string }) {
  const api = useMessagingApi()
  return <Templates api={api} org={org} server={server} />
}
export function ServerTemplates({ org, server }: { org: string; server: string }) {
  return (
    <MessagingApiProvider org={org} server={server}>
      <TemplatesBound org={org} server={server} />
    </MessagingApiProvider>
  )
}

// The messaging area no longer has a tab bar — it just provides the
// server messaging API context to its (single) page.
export function MessagingShell({
  org,
  server,
  children,
}: {
  org: string
  server: string
  children: React.ReactNode
}) {
  return (
    <MessagingApiProvider org={org} server={server}>
      {children}
    </MessagingApiProvider>
  )
}
