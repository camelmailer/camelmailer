"use client"

// The admin-API resource tabs of a mail server: domains, credentials,
// routes, webhooks, suppressions.

import { useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import {
  AtSignIcon,
  BanIcon,
  DownloadIcon,
  GlobeIcon,
  InboxIcon,
  KeyRoundIcon,
  PlusIcon,
  WebhookIcon,
} from "lucide-react"
import { toast } from "sonner"
import {
  ConfirmDialog,
  CopyButton,
  formatDate,
  PageHeader,
  SecretReveal,
} from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { FormDialog } from "@/components/form-dialog"
import { StatusPill } from "@/components/status-pill"
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
import { adminApi, ApiError, WEBHOOK_EVENTS, type Suppression } from "@/lib/api"
import {
  downloadFile,
  recipientHref,
  suppressionReasonText,
  suppressionsCsv,
  type SuppressionWithDate,
} from "@/lib/api-p2"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

type Scope = { org: string; server: string }

// ------------------------------------------------------------- domains

/// App route of the domain-detail view (records, health, delegation).
function domainHref(org: string, server: string, name: string): string {
  return `/orgs/${org}/servers/${server}/domains/${encodeURIComponent(name)}`
}

export function Domains({ org, server }: Scope) {
  const router = useRouter()
  const queryClient = useQueryClient()
  const key = ["domains", org, server]
  const domains = useQuery({ queryKey: key, queryFn: () => adminApi.domains(org, server).list() })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [deleteName, setDeleteName] = useState<string | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const create = useMutation({
    mutationFn: () => adminApi.domains(org, server).create(name),
    onSuccess: ({ domain }) => {
      invalidate()
      setOpen(false)
      setName("")
      // straight to the records that need publishing
      router.push(domainHref(org, server, domain.name))
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
        <EmptyState
          icon={GlobeIcon}
          title="No domains yet"
          description="Verify a domain so your mail authenticates with SPF and DKIM and lands in the inbox."
          action={{ label: "Add domain", onClick: () => setOpen(true) }}
        />
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
                <TableCell>
                  <Link
                    href={domainHref(org, server, domain.name)}
                    className="font-medium hover:underline"
                  >
                    {domain.name}
                  </Link>
                </TableCell>
                <TableCell>
                  <div className="flex flex-wrap gap-1.5">
                    <StatusPill status={domain.verified ? "verified" : "unverified"} />
                    {domain.dkim_record === null && <StatusPill status="no key" tone="amber" />}
                  </div>
                </TableCell>
                <TableCell className="space-x-2 text-right">
                  <Button variant="outline" size="sm" asChild>
                    <Link href={domainHref(org, server, domain.name)}>Records &amp; health</Link>
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
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title="Add sending domain"
        submitLabel="Add"
        onSubmit={() => create.mutate()}
        busy={create.isPending}
        submitDisabled={!name.includes(".")}
      >
        <div className="grid gap-2">
          <Label>Domain name</Label>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="mail.acme.com" />
        </div>
      </FormDialog>
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
      {credentials.isSuccess && credentials.data.credentials.length === 0 ? (
        <EmptyState
          icon={KeyRoundIcon}
          title="No credentials yet"
          description="Create an API key or SMTP credential so your application can send through this server."
          action={{ label: "New credential", onClick: () => { setIssued(null); setOpen(true) } }}
        />
      ) : (
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
      )}
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title="New credential"
        onSubmit={() => create.mutate()}
        busy={create.isPending}
        submitDisabled={!name.trim() || (type === "SMTP-IP" && !cidr)}
        showSubmit={!issued}
        cancelLabel={issued ? "Done" : "Cancel"}
      >
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
      </FormDialog>
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
        <EmptyState
          icon={InboxIcon}
          title="No inbound routes yet"
          description="Without a route, inbound mail to this server is rejected — add one to accept or forward it."
          action={{ label: "New route", onClick: () => setOpen(true) }}
        />
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
        <EmptyState
          icon={WebhookIcon}
          title="No webhooks yet"
          description="Get an HTTP callback the moment a message is sent, delayed, failed or held."
          action={{ label: "New webhook", onClick: () => setOpen(true) }}
        />
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
                <TableCell className="text-right">
                  <Button variant="ghost" size="sm" onClick={() => setDeleteId(webhook.id)}>
                    Delete
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title="New webhook"
        onSubmit={() => create.mutate()}
        busy={create.isPending}
        submitDisabled={!name.trim() || !url.startsWith("http")}
      >
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
      </FormDialog>
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
  const [reactivating, setReactivating] = useState<Suppression | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })
  const rows: SuppressionWithDate[] = suppressions.data?.suppressions ?? []

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
          <div className="flex items-center gap-2">
            {rows.length > 0 && (
              <Button
                variant="outline"
                size="sm"
                onClick={() =>
                  downloadFile(`suppressions-${server}.csv`, suppressionsCsv(rows))
                }
              >
                <DownloadIcon className="size-4" /> Export CSV
              </Button>
            )}
            <Button size="sm" onClick={() => setOpen(true)}>
              <PlusIcon className="size-4" /> Suppress address
            </Button>
          </div>
        }
      />
      {suppressions.data?.suppressions.length === 0 ? (
        <EmptyState
          icon={BanIcon}
          title="No suppressions yet"
          description="Hard-bouncing addresses land here automatically; you can also suppress addresses by hand."
          action={{ label: "Suppress address", onClick: () => setOpen(true) }}
        />
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Address</TableHead>
              <TableHead>Type</TableHead>
              <TableHead>Reason</TableHead>
              <TableHead>Date added</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {rows.map((suppression) => (
              <TableRow key={suppression.id}>
                <TableCell>
                  <Link
                    href={recipientHref(org, server, suppression.address)}
                    className="font-medium hover:underline"
                  >
                    {suppression.address}
                  </Link>
                </TableCell>
                <TableCell>
                  <Badge variant="outline">{suppression.type}</Badge>
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {suppressionReasonText(suppression)}
                </TableCell>
                <TableCell className="whitespace-nowrap text-muted-foreground">
                  {formatDate(suppression.created_at)}
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setReactivating(suppression)}
                  >
                    Reactivate
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      <ConfirmDialog
        open={reactivating !== null}
        onOpenChange={(open) => !open && setReactivating(null)}
        title={`Reactivate ${reactivating?.address}?`}
        description="The suppression is removed and this server delivers to the address again — make sure the underlying problem (bounce, complaint) is resolved."
        confirmLabel="Reactivate"
        onConfirm={async () => {
          try {
            await adminApi.suppressions(org, server).delete(reactivating!.address)
            invalidate()
            toast.success(`${reactivating!.address} reactivated`)
          } catch (err) {
            errorToast(err, "Could not reactivate the address")
          }
        }}
      />
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title="Suppress an address"
        submitLabel="Suppress"
        onSubmit={() => create.mutate()}
        busy={create.isPending}
        submitDisabled={!address.includes("@")}
      >
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
      </FormDialog>
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
        <EmptyState
          icon={AtSignIcon}
          title="No sender addresses yet"
          description="Authorize a single address to send from — no domain verification required."
          action={{ label: "Add address", onClick: () => { setIssuedToken(null); setOpen(true) } }}
        />
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
