"use client"

// The admin-API resource tabs of a mail server: domains, credentials,
// routes, webhooks, suppressions.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { PlusIcon } from "lucide-react"
import { toast } from "sonner"
import {
  ConfirmDialog,
  CopyButton,
  EmptyState,
  PageHeader,
  SecretReveal,
} from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import {
  adminApi,
  ApiError,
  WEBHOOK_EVENTS,
  type DnsRecord,
  type Domain,
  type Webhook,
  type WebhookTestResult,
} from "@/lib/api"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

type Scope = { org: string; server: string }

// ------------------------------------------------------------- domains

function DnsRecordRow({ label, record }: { label: string; record: DnsRecord }) {
  return (
    <div className="grid gap-1">
      <Label>
        {label} ({record.type})
      </Label>
      <div className="flex items-center gap-2">
        <code className="min-w-0 flex-1 break-all rounded bg-muted px-2 py-1 text-xs">
          {record.name}
        </code>
        <CopyButton value={record.name} />
      </div>
      <div className="flex items-center gap-2">
        <code className="min-w-0 flex-1 break-all rounded bg-muted px-2 py-1 text-xs">
          {record.value}
        </code>
        <CopyButton value={record.value} />
      </div>
    </div>
  )
}

export function Domains({ org, server }: Scope) {
  const queryClient = useQueryClient()
  const key = ["domains", org, server]
  const domains = useQuery({ queryKey: key, queryFn: () => adminApi.domains(org, server).list() })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [deleteName, setDeleteName] = useState<string | null>(null)
  const [recordsFor, setRecordsFor] = useState<Domain | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const create = useMutation({
    mutationFn: () => adminApi.domains(org, server).create(name),
    onSuccess: ({ domain }) => {
      invalidate()
      setOpen(false)
      setName("")
      setRecordsFor(domain)
    },
    onError: (err) => errorToast(err, "Could not add the domain"),
  })

  return (
    <div>
      <PageHeader
        title="Sending domains"
        description="Domains this server is allowed to send from (From/Sender authentication)."
        action={
          <Button size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> Add domain
          </Button>
        }
      />
      {domains.data?.domains.length === 0 ? (
        <EmptyState>No domains yet — add one to start sending.</EmptyState>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Domain</TableHead>
              <TableHead>Status</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {domains.data?.domains.map((domain) => (
              <TableRow key={domain.id}>
                <TableCell className="font-medium">{domain.name}</TableCell>
                <TableCell>
                  {domain.verified ? (
                    <Badge>verified</Badge>
                  ) : (
                    <Badge variant="secondary">unverified</Badge>
                  )}
                </TableCell>
                <TableCell className="space-x-2 text-right">
                  <Button variant="outline" size="sm" onClick={() => setRecordsFor(domain)}>
                    DNS records
                  </Button>
                  {!domain.verified && (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={async () => {
                        try {
                          await adminApi.domains(org, server).verify(domain.name)
                          invalidate()
                        } catch (err) {
                          errorToast(err, "Verification failed")
                        }
                      }}
                    >
                      Verify
                    </Button>
                  )}
                  <Button variant="ghost" size="sm" onClick={() => setDeleteName(domain.name)}>
                    Delete
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add sending domain</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            <Label>Domain name</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="mail.acme.com" />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button onClick={() => create.mutate()} disabled={create.isPending || !name.includes(".")}>
              Add
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <Dialog open={recordsFor !== null} onOpenChange={(open) => !open && setRecordsFor(null)}>
        <DialogContent className="sm:max-w-xl">
          <DialogHeader>
            <DialogTitle>DNS records for {recordsFor?.name}</DialogTitle>
          </DialogHeader>
          <p className="text-sm text-muted-foreground">
            Publish these TXT records, then hit Verify. The verification record proves
            ownership; SPF and DKIM authenticate the mail itself.
          </p>
          {recordsFor && (
            <div className="grid gap-4">
              <DnsRecordRow label="Verification" record={recordsFor.verification_record} />
              <DnsRecordRow label="SPF" record={recordsFor.spf_record} />
              {recordsFor.dkim_record && (
                <DnsRecordRow label="DKIM" record={recordsFor.dkim_record} />
              )}
            </div>
          )}
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={deleteName !== null}
        onOpenChange={(open) => !open && setDeleteName(null)}
        title={`Delete ${deleteName}?`}
        description="Mail from this domain will no longer authenticate."
        onConfirm={async () => {
          try {
            await adminApi.domains(org, server).delete(deleteName!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not delete the domain")
          }
        }}
      />
    </div>
  )
}

// --------------------------------------------------------- credentials

export function Credentials({ org, server }: Scope) {
  const queryClient = useQueryClient()
  const key = ["credentials", org, server]
  const credentials = useQuery({
    queryKey: key,
    queryFn: () => adminApi.credentials(org, server).list(),
  })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [type, setType] = useState("API")
  const [cidr, setCidr] = useState("")
  const [issued, setIssued] = useState<string | null>(null)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const create = useMutation({
    mutationFn: () =>
      adminApi.credentials(org, server).create({
        type,
        name,
        ...(type === "SMTP-IP" ? { key: cidr } : {}),
      }),
    onSuccess: ({ credential }) => {
      invalidate()
      setName("")
      if (credential.type !== "SMTP-IP") setIssued(credential.key)
      else setOpen(false)
    },
    onError: (err) => errorToast(err, "Could not create the credential"),
  })

  return (
    <div>
      <PageHeader
        title="Credentials"
        description="API keys (HTTP sending + messaging API) and SMTP credentials."
        action={
          <Button size="sm" onClick={() => { setIssued(null); setOpen(true) }}>
            <PlusIcon className="size-4" /> New credential
          </Button>
        }
      />
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Name</TableHead>
            <TableHead>Type</TableHead>
            <TableHead>Key</TableHead>
            <TableHead>Hold</TableHead>
            <TableHead />
          </TableRow>
        </TableHeader>
        <TableBody>
          {credentials.data?.credentials.map((credential) => (
            <TableRow key={credential.id}>
              <TableCell className="font-medium">{credential.name}</TableCell>
              <TableCell>
                <Badge variant="outline">{credential.type}</Badge>
              </TableCell>
              <TableCell>
                <span className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground">
                  {credential.key.slice(0, 8)}…
                  <CopyButton value={credential.key} />
                </span>
              </TableCell>
              <TableCell>
                <Switch
                  checked={credential.hold}
                  onCheckedChange={async (checked) => {
                    try {
                      await adminApi.credentials(org, server).update(credential.id, { hold: checked })
                      invalidate()
                    } catch (err) {
                      errorToast(err, "Could not update the credential")
                    }
                  }}
                />
              </TableCell>
              <TableCell className="text-right">
                <Button variant="ghost" size="sm" onClick={() => setDeleteId(credential.id)}>
                  Delete
                </Button>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New credential</DialogTitle>
          </DialogHeader>
          {issued ? (
            <SecretReveal label="Credential key" value={issued} />
          ) : (
            <div className="grid gap-4">
              <div className="grid gap-2">
                <Label>Name</Label>
                <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="backend" />
              </div>
              <div className="grid gap-2">
                <Label>Type</Label>
                <Select value={type} onValueChange={setType}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="API">API (HTTP)</SelectItem>
                    <SelectItem value="SMTP">SMTP (password)</SelectItem>
                    <SelectItem value="SMTP-IP">SMTP-IP (CIDR allowlist)</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              {type === "SMTP-IP" && (
                <div className="grid gap-2">
                  <Label>CIDR</Label>
                  <Input value={cidr} onChange={(e) => setCidr(e.target.value)} placeholder="10.0.0.0/8" />
                </div>
              )}
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              {issued ? "Done" : "Cancel"}
            </Button>
            {!issued && (
              <Button
                onClick={() => create.mutate()}
                disabled={create.isPending || !name.trim() || (type === "SMTP-IP" && !cidr)}
              >
                Create
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Delete credential"
        description="Clients using this credential will be rejected immediately."
        onConfirm={async () => {
          try {
            await adminApi.credentials(org, server).delete(deleteId!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not delete the credential")
          }
        }}
      />
    </div>
  )
}

// -------------------------------------------------------------- routes

export function Routes({ org, server }: Scope) {
  const queryClient = useQueryClient()
  const key = ["routes", org, server]
  const routes = useQuery({ queryKey: key, queryFn: () => adminApi.routes(org, server).list() })
  const domains = useQuery({
    queryKey: ["domains", org, server],
    queryFn: () => adminApi.domains(org, server).list(),
  })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [mode, setMode] = useState("Endpoint")
  const [domainId, setDomainId] = useState<string>("none")
  const [endpointUrl, setEndpointUrl] = useState("")
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const create = useMutation({
    mutationFn: () =>
      adminApi.routes(org, server).create({
        name,
        mode,
        ...(domainId !== "none" ? { domain_id: Number(domainId) } : {}),
        ...(mode === "Endpoint" && endpointUrl ? { endpoint_url: endpointUrl } : {}),
      }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setName("")
      setEndpointUrl("")
    },
    onError: (err) => errorToast(err, "Could not create the route"),
  })

  const domainName = (id: number | null) =>
    domains.data?.domains.find((domain) => domain.id === id)?.name ?? "route domain"

  return (
    <div>
      <PageHeader
        title="Inbound routes"
        description="What happens to mail arriving for an address on this server."
        action={
          <Button size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> New route
          </Button>
        }
      />
      {routes.data?.routes.length === 0 ? (
        <EmptyState>No routes — inbound mail is rejected.</EmptyState>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Address</TableHead>
              <TableHead>Mode</TableHead>
              <TableHead>Endpoint</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {routes.data?.routes.map((route) => (
              <TableRow key={route.id}>
                <TableCell className="font-medium">
                  {route.name}@{domainName(route.domain_id)}
                </TableCell>
                <TableCell>
                  <Badge variant="outline">{route.mode}</Badge>
                </TableCell>
                <TableCell className="max-w-64 truncate text-muted-foreground">
                  {route.endpoint_url ?? "—"}
                </TableCell>
                <TableCell className="text-right">
                  <Button variant="ghost" size="sm" onClick={() => setDeleteId(route.id)}>
                    Delete
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New inbound route</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid grid-cols-2 gap-2">
              <div className="grid gap-2">
                <Label>Local part</Label>
                <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="support or *" />
              </div>
              <div className="grid gap-2">
                <Label>Domain</Label>
                <Select value={domainId} onValueChange={setDomainId}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="none">(route domain)</SelectItem>
                    {domains.data?.domains.map((domain) => (
                      <SelectItem key={domain.id} value={domain.id.toString()}>
                        {domain.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            <div className="grid gap-2">
              <Label>Mode</Label>
              <Select value={mode} onValueChange={setMode}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {["Endpoint", "Accept", "Hold", "Bounce", "Reject"].map((m) => (
                    <SelectItem key={m} value={m}>
                      {m}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            {mode === "Endpoint" && (
              <div className="grid gap-2">
                <Label>HTTP endpoint URL</Label>
                <Input
                  value={endpointUrl}
                  onChange={(e) => setEndpointUrl(e.target.value)}
                  placeholder="https://app.acme.com/inbound"
                />
              </div>
            )}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button onClick={() => create.mutate()} disabled={create.isPending || !name.trim()}>
              Create
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Delete route"
        description="Mail to this address will follow the remaining routes or be rejected."
        onConfirm={async () => {
          try {
            await adminApi.routes(org, server).delete(deleteId!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not delete the route")
          }
        }}
      />
    </div>
  )
}

// ------------------------------------------------------------ webhooks

/// Details strings of the sample payload per event — mirrors the backend's
/// `sample_payload` so the JSON shown here matches what actually arrives.
const SAMPLE_DETAILS: Record<string, string> = {
  MessageSent: "Message for recipient@example.com accepted by mx.example.com",
  MessageDelayed: "421 4.7.0 try again later (attempt 2 of 18)",
  MessageDeliveryFailed: "550 5.1.1 recipient address rejected: user unknown",
  MessageHeld: "Message held for manual review (spam score above threshold)",
}

function samplePayload(event: string): string {
  return JSON.stringify(
    {
      event,
      timestamp: Math.floor(Date.now() / 1000),
      uuid: "<generated per delivery>",
      test: true,
      payload: {
        message: {
          id: 1234,
          token: "AbCdEf123456",
          rcpt_to: "recipient@example.com",
          mail_from: "sender@yourdomain.com",
          scope: "outgoing",
          bounce: false,
        },
        details: SAMPLE_DETAILS[event] ?? "Test delivery",
      },
    },
    null,
    2,
  )
}

function resultPill(result: WebhookTestResult) {
  if (result.delivered) {
    return <Badge>delivered · {result.status_code}</Badge>
  }
  if (result.status_code != null) {
    return <Badge variant="destructive">failed · HTTP {result.status_code}</Badge>
  }
  return <Badge variant="destructive">failed · no response</Badge>
}

/// "Send test": pick an event, POST the sample payload to the webhook URL
/// synchronously, show the outcome — plus the sample payload itself.
function SendTestDialog({
  org,
  server,
  webhook,
  onClose,
}: Scope & { webhook: Webhook; onClose: () => void }) {
  const [event, setEvent] = useState<string>(webhook.events[0] ?? "MessageSent")
  const [result, setResult] = useState<WebhookTestResult | null>(null)

  const send = useMutation({
    mutationFn: () => adminApi.webhooks(org, server).test(webhook.id, event),
    onSuccess: ({ result }) => setResult(result),
    onError: (err) => errorToast(err, "Could not send the test event"),
  })

  const payload = samplePayload(event)

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>Send a test to “{webhook.name}”</DialogTitle>
        </DialogHeader>
        <div className="grid gap-4">
          <p className="text-sm text-muted-foreground">
            Delivers one sample payload to <code className="text-xs">{webhook.url}</code> right
            now — with your custom headers{webhook.sign ? " and the RSA signature" : ""}, marked
            as <code className="text-xs">&quot;test&quot;: true</code>.
          </p>
          <div className="grid gap-2">
            <Label>Event</Label>
            <Select
              value={event}
              onValueChange={(value) => {
                setEvent(value)
                setResult(null)
              }}
            >
              <SelectTrigger className="w-64">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {WEBHOOK_EVENTS.map((name) => (
                  <SelectItem key={name} value={name}>
                    {name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className="grid gap-2">
            <div className="flex items-center justify-between">
              <Label>Example payload</Label>
              <CopyButton value={payload} />
            </div>
            <pre className="max-h-48 overflow-auto rounded-md bg-muted p-2 text-xs">
              {payload}
            </pre>
          </div>
          {result && (
            <div className="flex items-center gap-2 text-sm">
              {resultPill(result)}
              <span className="text-muted-foreground">{result.duration_ms} ms</span>
              {result.error && (
                <span className="break-all text-xs text-muted-foreground">{result.error}</span>
              )}
            </div>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Close
          </Button>
          <Button onClick={() => send.mutate()} disabled={send.isPending}>
            {send.isPending ? "Sending…" : "Send test"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

export function Webhooks({ org, server }: Scope) {
  const queryClient = useQueryClient()
  const key = ["webhooks", org, server]
  const webhooks = useQuery({ queryKey: key, queryFn: () => adminApi.webhooks(org, server).list() })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [url, setUrl] = useState("")
  const [sign, setSign] = useState(true)
  const [events, setEvents] = useState<string[]>([])
  const [headerRows, setHeaderRows] = useState<{ name: string; value: string }[]>([])
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const [testing, setTesting] = useState<Webhook | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const toggleEvent = (event: string, checked: boolean) =>
    setEvents((current) =>
      checked ? [...current, event] : current.filter((e) => e !== event),
    )
  const headersObject = () =>
    Object.fromEntries(
      headerRows
        .filter((row) => row.name.trim())
        .map((row) => [row.name.trim(), row.value]),
    )

  const create = useMutation({
    mutationFn: () =>
      adminApi.webhooks(org, server).create({
        name,
        url,
        sign,
        // no selection = subscribe to everything
        events,
        headers: headersObject(),
      }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setName("")
      setUrl("")
      setEvents([])
      setHeaderRows([])
    },
    onError: (err) => errorToast(err, "Could not create the webhook"),
  })

  return (
    <div>
      <PageHeader
        title="Webhooks"
        description="HTTP callbacks for message events (sent, delayed, failed, held)."
        action={
          <Button size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> New webhook
          </Button>
        }
      />
      {webhooks.data?.webhooks.length === 0 ? (
        <EmptyState>No webhooks configured.</EmptyState>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>URL</TableHead>
              <TableHead>Events</TableHead>
              <TableHead>Headers</TableHead>
              <TableHead>Signed</TableHead>
              <TableHead>Enabled</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {webhooks.data?.webhooks.map((webhook) => (
              <TableRow key={webhook.id}>
                <TableCell className="font-medium">{webhook.name}</TableCell>
                <TableCell className="max-w-64 truncate text-muted-foreground">
                  {webhook.url}
                </TableCell>
                <TableCell>
                  {webhook.events.length === 0 ? (
                    <Badge variant="secondary">all events</Badge>
                  ) : (
                    <div className="flex max-w-56 flex-wrap gap-1">
                      {webhook.events.map((event) => (
                        <Badge key={event} variant="outline">
                          {event}
                        </Badge>
                      ))}
                    </div>
                  )}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {Object.keys(webhook.headers ?? {}).length === 0
                    ? "—"
                    : Object.keys(webhook.headers).join(", ")}
                </TableCell>
                <TableCell>{webhook.sign ? <Badge variant="outline">RSA</Badge> : "—"}</TableCell>
                <TableCell>
                  <Switch
                    checked={webhook.enabled}
                    onCheckedChange={async (checked) => {
                      try {
                        if (checked) await adminApi.webhooks(org, server).enable(webhook.id)
                        else await adminApi.webhooks(org, server).disable(webhook.id)
                        invalidate()
                      } catch (err) {
                        errorToast(err, "Could not toggle the webhook")
                      }
                    }}
                  />
                </TableCell>
                <TableCell className="space-x-2 text-right">
                  <Button variant="outline" size="sm" onClick={() => setTesting(webhook)}>
                    Send test
                  </Button>
                  <Button variant="ghost" size="sm" onClick={() => setDeleteId(webhook.id)}>
                    Delete
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New webhook</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid gap-2">
              <Label>Name</Label>
              <Input value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <div className="grid gap-2">
              <Label>URL</Label>
              <Input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://…" />
            </div>
            <div className="grid gap-2">
              <Label>Events (none selected = all events)</Label>
              <div className="grid gap-1">
                {WEBHOOK_EVENTS.map((event) => (
                  <label key={event} className="flex items-center gap-2 text-sm">
                    <input
                      type="checkbox"
                      className="size-4 accent-primary"
                      checked={events.includes(event)}
                      onChange={(e) => toggleEvent(event, e.target.checked)}
                    />
                    {event}
                  </label>
                ))}
              </div>
            </div>
            <div className="grid gap-2">
              <Label>Custom headers (e.g. Authorization)</Label>
              {headerRows.map((row, index) => (
                <div key={index} className="flex items-center gap-2">
                  <Input
                    className="w-2/5"
                    placeholder="Header name"
                    value={row.name}
                    onChange={(e) =>
                      setHeaderRows((rows) =>
                        rows.map((r, i) => (i === index ? { ...r, name: e.target.value } : r)),
                      )
                    }
                  />
                  <Input
                    placeholder="Value"
                    value={row.value}
                    onChange={(e) =>
                      setHeaderRows((rows) =>
                        rows.map((r, i) => (i === index ? { ...r, value: e.target.value } : r)),
                      )
                    }
                  />
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() =>
                      setHeaderRows((rows) => rows.filter((_, i) => i !== index))
                    }
                  >
                    Remove
                  </Button>
                </div>
              ))}
              <Button
                variant="outline"
                size="sm"
                className="justify-self-start"
                onClick={() => setHeaderRows((rows) => [...rows, { name: "", value: "" }])}
              >
                <PlusIcon className="size-4" /> Add header
              </Button>
            </div>
            <div className="flex items-center gap-2">
              <Switch checked={sign} onCheckedChange={setSign} id="sign" />
              <Label htmlFor="sign">Sign payloads (RSA)</Label>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => create.mutate()}
              disabled={create.isPending || !name.trim() || !url.startsWith("http")}
            >
              Create
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {testing && (
        <SendTestDialog
          org={org}
          server={server}
          webhook={testing}
          onClose={() => setTesting(null)}
        />
      )}
      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Delete webhook"
        description="No further events will be delivered to this URL."
        onConfirm={async () => {
          try {
            await adminApi.webhooks(org, server).delete(deleteId!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not delete the webhook")
          }
        }}
      />
    </div>
  )
}

// -------------------------------------------------------- suppressions

export function Suppressions({ org, server }: Scope) {
  const queryClient = useQueryClient()
  const key = ["suppressions", org, server]
  const suppressions = useQuery({
    queryKey: key,
    queryFn: () => adminApi.suppressions(org, server).list(),
  })
  const [open, setOpen] = useState(false)
  const [address, setAddress] = useState("")
  const [reason, setReason] = useState("")
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const create = useMutation({
    mutationFn: () =>
      adminApi.suppressions(org, server).create({
        type: "recipient",
        address,
        ...(reason ? { reason } : {}),
      }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setAddress("")
      setReason("")
    },
    onError: (err) => errorToast(err, "Could not add the suppression"),
  })

  return (
    <div>
      <PageHeader
        title="Suppression list"
        description="Addresses this server will not deliver to (holds instead)."
        action={
          <Button size="sm" onClick={() => setOpen(true)}>
            <PlusIcon className="size-4" /> Suppress address
          </Button>
        }
      />
      {suppressions.data?.suppressions.length === 0 ? (
        <EmptyState>The suppression list is empty.</EmptyState>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Address</TableHead>
              <TableHead>Type</TableHead>
              <TableHead>Reason</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {suppressions.data?.suppressions.map((suppression) => (
              <TableRow key={suppression.id}>
                <TableCell className="font-medium">{suppression.address}</TableCell>
                <TableCell>
                  <Badge variant="outline">{suppression.type}</Badge>
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {suppression.reason ?? "—"}
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={async () => {
                      try {
                        await adminApi.suppressions(org, server).delete(suppression.address)
                        invalidate()
                      } catch (err) {
                        errorToast(err, "Could not remove the suppression")
                      }
                    }}
                  >
                    Remove
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Suppress an address</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid gap-2">
              <Label>Email address</Label>
              <Input value={address} onChange={(e) => setAddress(e.target.value)} />
            </div>
            <div className="grid gap-2">
              <Label>Reason (optional)</Label>
              <Input value={reason} onChange={(e) => setReason(e.target.value)} />
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => create.mutate()}
              disabled={create.isPending || !address.includes("@")}
            >
              Suppress
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}


// ----------------------------------------------------- sender addresses

export function SenderAddresses({ org, server }: Scope) {
  const queryClient = useQueryClient()
  const key = ["sender-addresses", org, server]
  const addresses = useQuery({
    queryKey: key,
    queryFn: () => adminApi.senderAddresses(org, server).list(),
  })
  const [open, setOpen] = useState(false)
  const [email, setEmail] = useState("")
  const [issuedToken, setIssuedToken] = useState<string | null>(null)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const create = useMutation({
    mutationFn: () => adminApi.senderAddresses(org, server).create(email),
    onSuccess: ({ verification_token }) => {
      invalidate()
      setEmail("")
      if (verification_token) {
        // shown exactly once: this instance couldn't email the link
        setIssuedToken(verification_token)
      } else {
        setOpen(false)
        toast.success("Confirmation email sent")
      }
    },
    onError: (err) => errorToast(err, "Could not add the sender address"),
  })

  return (
    <div>
      <PageHeader
        title="Sender addresses"
        description="Individual addresses this server may send from once their owner confirms them — no domain verification needed."
        action={
          <Button size="sm" onClick={() => { setIssuedToken(null); setOpen(true) }}>
            <PlusIcon className="size-4" /> Add address
          </Button>
        }
      />
      {addresses.data?.sender_addresses.length === 0 ? (
        <EmptyState>No sender addresses yet.</EmptyState>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Email address</TableHead>
              <TableHead>Status</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {addresses.data?.sender_addresses.map((address) => (
              <TableRow key={address.id}>
                <TableCell className="font-medium">{address.email_address}</TableCell>
                <TableCell>
                  {address.verified ? (
                    <Badge>confirmed</Badge>
                  ) : (
                    <Badge variant="secondary">pending</Badge>
                  )}
                </TableCell>
                <TableCell className="text-right">
                  <Button variant="ghost" size="sm" onClick={() => setDeleteId(address.id)}>
                    Delete
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add sender address</DialogTitle>
          </DialogHeader>
          {issuedToken ? (
            <div className="grid gap-2">
              <p className="text-sm text-muted-foreground">
                This instance can&apos;t email the confirmation link. Relay this one-time
                token to the address owner — they confirm at
                {" "}<code className="text-xs">/sender-addresses/confirm</code>.
              </p>
              <SecretReveal label="Verification token" value={issuedToken} />
            </div>
          ) : (
            <div className="grid gap-2">
              <Label>Email address</Label>
              <Input
                type="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder="person@partner.example"
              />
              <p className="text-xs text-muted-foreground">
                A confirmation link is sent to exactly this address.
              </p>
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              {issuedToken ? "Done" : "Cancel"}
            </Button>
            {!issuedToken && (
              <Button
                onClick={() => create.mutate()}
                disabled={create.isPending || !email.includes("@")}
              >
                Add
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Delete sender address"
        description="Mail from this exact address will no longer be authorized."
        onConfirm={async () => {
          try {
            await adminApi.senderAddresses(org, server).delete(deleteId!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not delete the sender address")
          }
        }}
      />
    </div>
  )
}
