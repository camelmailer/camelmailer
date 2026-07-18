"use client"

// The admin-API resource tabs of a mail server: domains, credentials,
// routes, webhooks, suppressions.

import { useEffect, useState } from "react"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import {
  AtSignIcon,
  BanIcon,
  DownloadIcon,
  GlobeIcon,
  InboxIcon,
  KeyRoundIcon,
  PlusIcon,
  SearchIcon,
  UploadIcon,
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
import { DataExportDialog, type ExportColumn } from "@/components/data-export-dialog"
import {
  DataImportDialog,
  runRowImport,
  type ImportColumn,
} from "@/components/data-import-dialog"
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
import { DataTable } from "@/components/ui/data-table"
import { Page } from "@/components/page"
import {
  adminApi,
  ApiError,
  WEBHOOK_EVENTS,
  type Domain,
  type Route,
  type SenderAddress,
  type Suppression,
  type Webhook,
  type WebhookTestResult,
} from "@/lib/api"
import {
  recipientHref,
  suppressionReasonText,
  type SuppressionWithDate,
} from "@/lib/api-p2"
import { relativeTime } from "@/lib/api-p1"
import {
  deriveSmtpHost,
  maskKey,
  SMTP_PORTS,
  smtpUsername,
  WEBHOOK_EVENT_META,
  webhookSamplePayload,
  type CredentialWithUsage,
} from "@/lib/api-p3"
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
  const [exportOpen, setExportOpen] = useState(false)
  const [importOpen, setImportOpen] = useState(false)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })
  const rows = domains.data?.domains ?? []

  const exportColumns: ExportColumn<Domain>[] = [
    { key: "name", label: "Domain", accessor: (d) => d.name },
    { key: "verified", label: "Verified", accessor: (d) => d.verified },
    { key: "dkim_configured", label: "DKIM configured", accessor: (d) => d.dkim_record !== null },
  ]
  const importColumns: ImportColumn[] = [
    { key: "name", label: "Domain", example: "mail.acme.com", required: true },
  ]

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

  const columns: ColumnDef<Domain>[] = [
    {
      id: "domain",
      header: "Domain",
      accessorFn: (d) => d.name,
      cell: ({ row }) => (
        <Link
          href={domainHref(org, server, row.original.name)}
          className="block max-w-[24rem] truncate font-medium transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.name}
        </Link>
      ),
    },
    {
      id: "status",
      header: "Status",
      enableSorting: false,
      accessorFn: (d) => (d.verified ? "verified" : "unverified"),
      filterFn: (row, _id, value) =>
        (row.original.verified ? "verified" : "unverified") === value,
      cell: ({ row }) => (
        <div className="flex flex-wrap gap-1.5">
          <StatusPill status={row.original.verified ? "verified" : "unverified"} />
          {row.original.dkim_record === null && <StatusPill status="no key" tone="amber" />}
        </div>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => {
        const domain = row.original
        return (
          <div className="space-x-2">
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
          </div>
        )
      },
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Domains"
          description="Domains this server is allowed to send from, authenticated with SPF and DKIM."
          className="mb-0"
          action={
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
                <UploadIcon className="size-4" /> Import
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setExportOpen(true)}
                disabled={rows.length === 0}
              >
                <DownloadIcon className="size-4" /> Export
              </Button>
              <Button size="sm" onClick={() => setOpen(true)}>
                <PlusIcon className="size-4" /> Add domain
              </Button>
            </div>
          }
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {domains.data?.domains.length === 0 ? (
          <EmptyState
            icon={GlobeIcon}
            title="No domains yet"
            description="Verify a domain so your mail authenticates with SPF and DKIM and lands in the inbox."
            action={{ label: "Add domain", onClick: () => setOpen(true) }}
          />
        ) : (
          <DataTable
            columns={columns}
            data={domains.data?.domains ?? []}
            loading={domains.isPending}
            searchKeys={["name"]}
            searchPlaceholder="Search domains…"
            emptyText="No domains match your search."
            initialPageSize={20}
            fillHeight
            filters={[
              {
                columnId: "status",
                label: "Status",
                options: [
                  { label: "Verified", value: "verified" },
                  { label: "Unverified", value: "unverified" },
                ],
              },
            ]}
          />
        )}
      </div>
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
      <DataExportDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        title="Export domains"
        filename={`domains-${server}`}
        columns={exportColumns}
        rows={rows}
      />
      <DataImportDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        title="Import domains"
        templateFilename="domains-template"
        columns={importColumns}
        onDone={invalidate}
        onImport={(imported, onProgress) =>
          runRowImport(
            imported,
            (r) => adminApi.domains(org, server).create(r.name),
            onProgress,
          )
        }
      />
    </Page>
  )
}

// --------------------------------------------------------- credentials

/// The last-used cell: relative time, or "No activity" for a key that has
/// never authenticated a request (masterplan §4.9 key hygiene).
function LastUsed({ at }: { at: string | null | undefined }) {
  if (!at) {
    return (
      <span className="inline-flex items-center gap-1 text-muted-foreground" title="This key has never authenticated a request">
        No activity
      </span>
    )
  }
  return (
    <span className="whitespace-nowrap text-muted-foreground" title={formatDate(at)}>
      {relativeTime(at)}
    </span>
  )
}

/// One copy-first row of the SMTP settings panel.
function CopyField({ label, value }: { label: string; value: string }) {
  return (
    <div className="grid gap-1">
      <Label className="text-xs text-muted-foreground">{label}</Label>
      <div className="flex items-center gap-2">
        <code className="min-w-0 flex-1 truncate rounded bg-muted px-2 py-1.5 font-mono text-xs">
          {value}
        </code>
        <CopyButton value={value} />
      </div>
    </div>
  )
}

/// Credential info as a lightbox (no detail page): status, the key (or
/// allowed CIDR) and — for SMTP credentials — the copy-first connection
/// facts (host, ports, username = `org/server`, password = the key).
function CredentialDialog({
  org,
  server,
  credential,
  smtpHost,
  onClose,
}: Scope & { credential: CredentialWithUsage; smtpHost: string; onClose: () => void }) {
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle className="flex flex-wrap items-center gap-2">
            {credential.name}
            <Badge variant="outline">{credential.type}</Badge>
            <StatusPill
              status={credential.hold ? "On hold" : "Active"}
              tone={credential.hold ? "amber" : undefined}
            />
          </DialogTitle>
        </DialogHeader>
        <div className="grid gap-3">
          {credential.type === "SMTP-IP" ? (
            <CopyField label="Allowed CIDR" value={credential.key ?? ""} />
          ) : (
            <CopyField label="Key" value={credential.key ?? ""} />
          )}

          {credential.type === "SMTP" && (
            <>
              <div className="mt-1 border-t pt-3 text-sm font-medium">SMTP settings</div>
              <CopyField label="Host" value={smtpHost} />
              <div className="grid gap-1">
                <Label className="text-xs text-muted-foreground">Ports</Label>
                <div className="flex flex-wrap gap-1.5">
                  {SMTP_PORTS.map((p) => (
                    <Badge key={p.port} variant="outline" className="font-mono">
                      {p.port}
                      <span className="ml-1 font-sans font-normal text-muted-foreground">
                        {p.note}
                      </span>
                    </Badge>
                  ))}
                </div>
              </div>
              <CopyField label="Username" value={smtpUsername(org, server)} />
              <CopyField label="Password" value={credential.key ?? ""} />
              <p className="rounded-md border bg-muted/40 p-2 text-xs text-muted-foreground">
                Your password is this credential&apos;s key. There is no separate SMTP password. Use
                port 587 with STARTTLS unless your client needs implicit TLS (465).
              </p>
            </>
          )}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Close
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

/// The three credential kinds, explained inside the create dialog.
const CREDENTIAL_CAPABILITIES = [
  {
    name: "Server key (API / SMTP)",
    detail: "Sends mail and reads this one server's messaging API. What you create here.",
  },
  {
    name: "Admin key",
    detail:
      "Full management access across the whole installation, machine to machine. Created under Admin.",
  },
  {
    name: "User session",
    detail: "Your signed-in browser session, scoped by your organization role (viewer → owner).",
  },
]

export function Credentials({ org, server }: Scope) {
  const queryClient = useQueryClient()
  const key = ["credentials", org, server]
  const credentials = useQuery({
    queryKey: key,
    queryFn: () => adminApi.credentials(org, server).list(),
  })
  const domains = useQuery({
    queryKey: ["domains", org, server],
    queryFn: () => adminApi.domains(org, server).list(),
  })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [type, setType] = useState("API")
  const [cidr, setCidr] = useState("")
  const [issued, setIssued] = useState<string | null>(null)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const [viewing, setViewing] = useState<CredentialWithUsage | null>(null)
  const [exportOpen, setExportOpen] = useState(false)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  // Metadata only: the secret key is never exported.
  const exportColumns: ExportColumn<CredentialWithUsage>[] = [
    { key: "name", label: "Name", accessor: (c) => c.name },
    { key: "type", label: "Type", accessor: (c) => c.type },
    { key: "last_used_at", label: "Last used", accessor: (c) => c.last_used_at ?? "" },
    { key: "hold", label: "Hold", accessor: (c) => c.hold },
  ]

  const smtpHost =
    typeof window !== "undefined"
      ? deriveSmtpHost(domains.data?.domains, window.location.hostname)
      : deriveSmtpHost(domains.data?.domains, "smtp.camelmailer.com")
  const rows = (credentials.data?.credentials ?? []) as CredentialWithUsage[]

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

  const columns: ColumnDef<CredentialWithUsage>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (c) => c.name,
      cell: ({ row }) => (
        <button
          type="button"
          onClick={() => setViewing(row.original)}
          className="block max-w-[20rem] truncate text-left font-medium transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.name}
        </button>
      ),
    },
    {
      id: "type",
      header: "Type",
      accessorFn: (c) => c.type,
      filterFn: (row, _id, value) => row.original.type === value,
      cell: ({ row }) => <Badge variant="outline">{row.original.type}</Badge>,
    },
    {
      id: "key",
      header: "Key",
      enableSorting: false,
      cell: ({ row }) => {
        const credential = row.original
        return credential.type === "SMTP-IP" ? (
          <span className="font-mono text-xs text-muted-foreground">{credential.key ?? ""}</span>
        ) : (
          <span className="inline-flex items-center gap-1 font-mono text-xs text-muted-foreground">
            {maskKey(credential.key)}
            <CopyButton value={credential.key ?? ""} />
          </span>
        )
      },
    },
    {
      id: "lastUsed",
      header: "Last used",
      accessorFn: (c) => c.last_used_at ?? "",
      cell: ({ row }) => <LastUsed at={row.original.last_used_at} />,
    },
    {
      id: "hold",
      header: "Hold",
      enableSorting: false,
      cell: ({ row }) => {
        const credential = row.original
        return (
          <div className="flex items-center gap-2">
            <StatusPill
              status={credential.hold ? "On hold" : "Active"}
              tone={credential.hold ? "amber" : undefined}
            />
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
          </div>
        )
      },
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => {
        const credential = row.original
        return (
          <div className="space-x-2">
            <Button variant="outline" size="sm" onClick={() => setViewing(credential)}>
              Details
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setDeleteId(credential.id)}>
              Delete
            </Button>
          </div>
        )
      },
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Credentials"
          description="API keys for the HTTP sending and messaging API, plus SMTP credentials for this server."
          className="mb-0"
          action={
            <div className="flex items-center gap-2">
              <Button
                variant="outline"
                size="sm"
                onClick={() => setExportOpen(true)}
                disabled={rows.length === 0}
              >
                <DownloadIcon className="size-4" /> Export
              </Button>
              <Button size="sm" onClick={() => { setIssued(null); setOpen(true) }}>
                <PlusIcon className="size-4" /> New credential
              </Button>
            </div>
          }
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {credentials.isSuccess && rows.length === 0 ? (
          <EmptyState
            icon={KeyRoundIcon}
            title="No credentials yet"
            description="Create an API key or SMTP credential so your application can send through this server."
            action={{ label: "New credential", onClick: () => { setIssued(null); setOpen(true) } }}
          />
        ) : (
          <DataTable
            columns={columns}
            data={rows}
            loading={credentials.isPending}
            searchKeys={["name"]}
            searchPlaceholder="Search credentials…"
            emptyText="No credentials match your search."
            initialPageSize={20}
            fillHeight
            filters={[
              {
                columnId: "type",
                label: "Type",
                options: [
                  { label: "API", value: "API" },
                  { label: "SMTP", value: "SMTP" },
                  { label: "SMTP-IP", value: "SMTP-IP" },
                ],
              },
            ]}
          />
        )}
      </div>
      {viewing && (
        <CredentialDialog
          org={org}
          server={server}
          credential={viewing}
          smtpHost={smtpHost}
          onClose={() => setViewing(null)}
        />
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
            <div className="grid gap-2 rounded-md border bg-muted/30 p-3">
              <p className="text-xs font-medium text-muted-foreground">
                Which key does what
              </p>
              {CREDENTIAL_CAPABILITIES.map((c) => (
                <div key={c.name} className="grid gap-0.5">
                  <p className="text-xs font-medium">{c.name}</p>
                  <p className="text-xs text-muted-foreground">{c.detail}</p>
                </div>
              ))}
            </div>
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
      <DataExportDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        title="Export credentials"
        description="Credential metadata only. Secret keys are never exported."
        filename={`credentials-${server}`}
        columns={exportColumns}
        rows={rows}
      />
    </Page>
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
  const [editId, setEditId] = useState<number | null>(null)
  const [name, setName] = useState("")
  const [mode, setMode] = useState("Endpoint")
  const [domainId, setDomainId] = useState<string>("none")
  const [endpointUrl, setEndpointUrl] = useState("")
  const [token, setToken] = useState<string | null>(null)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const [exportOpen, setExportOpen] = useState(false)
  const [importOpen, setImportOpen] = useState(false)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })
  const rows = routes.data?.routes ?? []

  function openCreate() {
    setEditId(null)
    setName("")
    setMode("Endpoint")
    setDomainId("none")
    setEndpointUrl("")
    setToken(null)
    setOpen(true)
  }
  function openEdit(route: Route) {
    setEditId(route.id)
    setName(route.name)
    setMode(route.mode)
    setDomainId(route.domain_id?.toString() ?? "none")
    setEndpointUrl(route.endpoint_url ?? "")
    setToken(route.token)
    setOpen(true)
  }

  const save = useMutation({
    mutationFn: () => {
      const routesApi = adminApi.routes(org, server)
      if (editId !== null) {
        // The API only updates name + mode; domain and endpoint are fixed at creation.
        return routesApi.update(editId, { name, mode })
      }
      const domainName = domains.data?.domains.find((d) => d.id.toString() === domainId)?.name
      return routesApi.create({
        name,
        mode,
        ...(domainName ? { domain: domainName } : {}),
        ...(mode === "Endpoint" && endpointUrl ? { endpoint_url: endpointUrl } : {}),
      })
    },
    onSuccess: () => {
      invalidate()
      setOpen(false)
    },
    onError: (err) =>
      errorToast(err, editId !== null ? "Could not save the route" : "Could not create the route"),
  })

  const domainName = (id: number | null) =>
    domains.data?.domains.find((domain) => domain.id === id)?.name ?? "route domain"

  const exportColumns: ExportColumn<Route>[] = [
    { key: "name", label: "Local part", accessor: (r) => r.name },
    { key: "domain", label: "Domain", accessor: (r) => domainName(r.domain_id) },
    { key: "mode", label: "Mode", accessor: (r) => r.mode },
    { key: "endpoint_url", label: "Endpoint", accessor: (r) => r.endpoint_url ?? "" },
  ]
  const importColumns: ImportColumn[] = [
    { key: "name", label: "Local part", example: "support", required: true },
    { key: "mode", label: "Mode", example: "Endpoint", required: true },
    { key: "domain", label: "Domain", example: "mail.acme.com" },
    { key: "endpoint_url", label: "Endpoint URL", example: "https://app.acme.com/inbound" },
  ]

  const columns: ColumnDef<Route>[] = [
    {
      id: "address",
      header: "Address",
      accessorFn: (r) => `${r.name}@${domainName(r.domain_id)}`,
      cell: ({ row }) => (
        <button
          type="button"
          onClick={() => openEdit(row.original)}
          className="block max-w-[24rem] truncate text-left font-medium transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.name}@{domainName(row.original.domain_id)}
        </button>
      ),
    },
    {
      id: "mode",
      header: "Mode",
      accessorFn: (r) => r.mode,
      filterFn: (row, _id, value) => row.original.mode === value,
      cell: ({ row }) => <Badge variant="outline">{row.original.mode}</Badge>,
    },
    {
      id: "endpoint",
      header: "Endpoint",
      enableSorting: false,
      accessorFn: (r) => r.endpoint_url ?? "",
      cell: ({ row }) => (
        <span className="block max-w-64 truncate text-muted-foreground">
          {row.original.endpoint_url ?? "—"}
        </span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <div className="space-x-2">
          <Button variant="outline" size="sm" onClick={() => openEdit(row.original)}>
            Details
          </Button>
          <Button variant="ghost" size="sm" onClick={() => setDeleteId(row.original.id)}>
            Delete
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
          title="Routes"
          description="Where inbound mail for this server's addresses goes: accept, hold, bounce or forward to an HTTP endpoint."
          className="mb-0"
          action={
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
                <UploadIcon className="size-4" /> Import
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setExportOpen(true)}
                disabled={rows.length === 0}
              >
                <DownloadIcon className="size-4" /> Export
              </Button>
              <Button size="sm" onClick={openCreate}>
                <PlusIcon className="size-4" /> New route
              </Button>
            </div>
          }
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {routes.data?.routes.length === 0 ? (
          <EmptyState
            icon={InboxIcon}
            title="No inbound routes yet"
            description="Without a route, inbound mail to this server is rejected. Add one to accept or forward it."
            action={{ label: "New route", onClick: openCreate }}
          />
        ) : (
          <DataTable
            columns={columns}
            data={routes.data?.routes ?? []}
            loading={routes.isPending}
            searchKeys={["name", "endpoint_url"]}
            searchPlaceholder="Search routes…"
            emptyText="No routes match your search."
            initialPageSize={20}
            fillHeight
            filters={[
              {
                columnId: "mode",
                label: "Mode",
                options: ["Endpoint", "Accept", "Hold", "Bounce", "Reject"].map((m) => ({
                  label: m,
                  value: m,
                })),
              },
            ]}
          />
        )}
      </div>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{editId !== null ? "Route details" : "New route"}</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid grid-cols-2 gap-2">
              <div className="grid gap-2">
                <Label>Local part</Label>
                <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="support or *" />
              </div>
              <div className="grid gap-2">
                <Label>Domain</Label>
                <Select value={domainId} onValueChange={setDomainId} disabled={editId !== null}>
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
                  disabled={editId !== null}
                />
              </div>
            )}
            {editId !== null && (
              <p className="text-xs text-muted-foreground">
                Domain and endpoint are set at creation. To change them, delete this route and
                create a new one.
              </p>
            )}
            {editId !== null && token && <CopyField label="Token" value={token} />}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button onClick={() => save.mutate()} disabled={save.isPending || !name.trim()}>
              {save.isPending
                ? "Saving…"
                : editId !== null
                  ? "Save"
                  : "Create"}
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
      <DataExportDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        title="Export routes"
        filename={`routes-${server}`}
        columns={exportColumns}
        rows={rows}
      />
      <DataImportDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        title="Import routes"
        templateFilename="routes-template"
        columns={importColumns}
        onDone={invalidate}
        onImport={(imported, onProgress) =>
          runRowImport(
            imported,
            (r) =>
              adminApi.routes(org, server).create({
                name: r.name,
                mode: r.mode || "Endpoint",
                ...(r.domain ? { domain: r.domain } : {}),
                ...(r.endpoint_url ? { endpoint_url: r.endpoint_url } : {}),
              }),
            onProgress,
          )
        }
      />
    </Page>
  )
}

// ------------------------------------------------------------ webhooks

/// App route of the webhook-detail view (endpoint, events, headers, edit).
function webhookHref(org: string, server: string, id: number): string {
  return `/orgs/${org}/servers/${server}/webhooks/${id}`
}

/// A soft, tinted event chip using the shared lifecycle color semantics
/// (sent green · delayed/held amber · failed red).
function EventPill({ event }: { event: string }) {
  const meta = WEBHOOK_EVENT_META[event]
  return <StatusPill status={meta?.label ?? event} tone={meta?.tone ?? "gray"} />
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

/// Live example-payload preview for the webhook editor: pick one of the
/// subscribed events (or any event when none are selected) and see exactly
/// the JSON body that will POST to the endpoint, with a Copy button.
function WebhookPayloadPreview({ events }: { events: string[] }) {
  const choices = events.length > 0 ? events : [...WEBHOOK_EVENTS]
  const [event, setEvent] = useState(choices[0])
  const active = choices.includes(event) ? event : choices[0]
  const payload = webhookSamplePayload(active)
  return (
    <div className="grid gap-2 rounded-md border bg-muted/30 p-2">
      <div className="flex items-center justify-between gap-2">
        <div className="flex items-center gap-2">
          <Label className="text-xs">Example payload</Label>
          <Select value={active} onValueChange={setEvent}>
            <SelectTrigger size="sm" className="h-7 w-48">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {choices.map((name) => (
                <SelectItem key={name} value={name}>
                  {name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <CopyButton value={payload} />
      </div>
      <pre className="max-h-44 overflow-auto rounded bg-background p-2 text-xs">{payload}</pre>
    </div>
  )
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

  const payload = webhookSamplePayload(event)

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-xl">
        <DialogHeader>
          <DialogTitle>Send a test to “{webhook.name}”</DialogTitle>
        </DialogHeader>
        <div className="grid gap-4">
          <p className="text-sm text-muted-foreground">
            Delivers one sample payload to <code className="text-xs">{webhook.url}</code> right
            now, with your custom headers{webhook.sign ? " and the RSA signature" : ""}, marked
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
  const [eventSearch, setEventSearch] = useState("")
  const [headerRows, setHeaderRows] = useState<{ name: string; value: string }[]>([])
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const [testing, setTesting] = useState<Webhook | null>(null)
  const [exportOpen, setExportOpen] = useState(false)
  const [importOpen, setImportOpen] = useState(false)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })
  const rows = webhooks.data?.webhooks ?? []

  const exportColumns: ExportColumn<Webhook>[] = [
    { key: "name", label: "Name", accessor: (w) => w.name },
    { key: "url", label: "URL", accessor: (w) => w.url },
    { key: "events", label: "Events", accessor: (w) => w.events.join(";") },
    { key: "enabled", label: "Enabled", accessor: (w) => w.enabled },
    { key: "signed", label: "Signed", accessor: (w) => w.sign },
  ]
  const importColumns: ImportColumn[] = [
    { key: "name", label: "Name", example: "Order events", required: true },
    { key: "url", label: "URL", example: "https://app.acme.com/hooks", required: true },
    { key: "events", label: "Events (; separated, blank = all)", example: "MessageSent;MessageDeliveryFailed" },
    { key: "sign", label: "Sign (true / false)", example: "true" },
  ]

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

  const columns: ColumnDef<Webhook>[] = [
    {
      id: "name",
      header: "Name",
      accessorFn: (w) => w.name,
      cell: ({ row }) => (
        <Link
          href={webhookHref(org, server, row.original.id)}
          className="block max-w-[16rem] truncate font-medium transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.name}
        </Link>
      ),
    },
    {
      id: "url",
      header: "URL",
      enableSorting: false,
      accessorFn: (w) => w.url,
      cell: ({ row }) => (
        <span className="block max-w-64 truncate text-muted-foreground">{row.original.url}</span>
      ),
    },
    {
      id: "events",
      header: "Events",
      enableSorting: false,
      cell: ({ row }) =>
        row.original.events.length === 0 ? (
          <Badge variant="secondary">all events</Badge>
        ) : (
          <div className="flex max-w-56 flex-wrap gap-1">
            {row.original.events.map((event) => (
              <EventPill key={event} event={event} />
            ))}
          </div>
        ),
    },
    {
      id: "headers",
      header: "Headers",
      enableSorting: false,
      cell: ({ row }) => (
        <span className="text-muted-foreground">
          {Object.keys(row.original.headers ?? {}).length === 0
            ? "—"
            : Object.keys(row.original.headers).join(", ")}
        </span>
      ),
    },
    {
      id: "signed",
      header: "Signed",
      enableSorting: false,
      cell: ({ row }) =>
        row.original.sign ? <Badge variant="outline">RSA</Badge> : <span>—</span>,
    },
    {
      id: "enabled",
      header: "Enabled",
      enableSorting: false,
      accessorFn: (w) => (w.enabled ? "enabled" : "disabled"),
      filterFn: (row, _id, value) =>
        (row.original.enabled ? "enabled" : "disabled") === value,
      cell: ({ row }) => {
        const webhook = row.original
        return (
          <div className="flex items-center gap-2">
            <StatusPill status={webhook.enabled ? "Active" : "Disabled"} />
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
          </div>
        )
      },
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => {
        const webhook = row.original
        return (
          <div className="space-x-2">
            <Button variant="outline" size="sm" onClick={() => setTesting(webhook)}>
              Send test
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setDeleteId(webhook.id)}>
              Delete
            </Button>
          </div>
        )
      },
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Webhooks"
          description="HTTP callbacks for message events (sent, delayed, failed, held)."
          className="mb-0"
          action={
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
                <UploadIcon className="size-4" /> Import
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setExportOpen(true)}
                disabled={rows.length === 0}
              >
                <DownloadIcon className="size-4" /> Export
              </Button>
              <Button size="sm" onClick={() => setOpen(true)}>
                <PlusIcon className="size-4" /> New webhook
              </Button>
            </div>
          }
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {webhooks.data?.webhooks.length === 0 ? (
          <EmptyState
            icon={WebhookIcon}
            title="No webhooks yet"
            description="Get an HTTP callback the moment a message is sent, delayed, failed or held."
            action={{ label: "New webhook", onClick: () => setOpen(true) }}
          />
        ) : (
          <DataTable
            columns={columns}
            data={webhooks.data?.webhooks ?? []}
            loading={webhooks.isPending}
            searchKeys={["name", "url"]}
            searchPlaceholder="Search webhooks…"
            emptyText="No webhooks match your search."
            initialPageSize={20}
            fillHeight
            filters={[
              {
                columnId: "enabled",
                label: "State",
                options: [
                  { label: "Enabled", value: "enabled" },
                  { label: "Disabled", value: "disabled" },
                ],
              },
            ]}
          />
        )}
      </div>
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
              <div className="relative">
                <SearchIcon className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  className="pl-8"
                  placeholder="Search events…"
                  value={eventSearch}
                  onChange={(e) => setEventSearch(e.target.value)}
                />
              </div>
              <div className="grid gap-1 rounded-md border p-2">
                {WEBHOOK_EVENTS.filter((event) =>
                  event.toLowerCase().includes(eventSearch.toLowerCase()),
                ).map((event) => (
                  <label
                    key={event}
                    className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-sm hover:bg-muted/60"
                  >
                    <input
                      type="checkbox"
                      className="size-4 accent-primary"
                      checked={events.includes(event)}
                      onChange={(e) => toggleEvent(event, e.target.checked)}
                    />
                    <EventPill event={event} />
                    <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                      {WEBHOOK_EVENT_META[event]?.description}
                    </span>
                  </label>
                ))}
              </div>
              <WebhookPayloadPreview events={events} />
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
      <DataExportDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        title="Export webhooks"
        filename={`webhooks-${server}`}
        columns={exportColumns}
        rows={rows}
      />
      <DataImportDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        title="Import webhooks"
        templateFilename="webhooks-template"
        columns={importColumns}
        onDone={invalidate}
        onImport={(imported, onProgress) =>
          runRowImport(
            imported,
            (r) =>
              adminApi.webhooks(org, server).create({
                name: r.name,
                url: r.url,
                sign: r.sign ? r.sign.trim().toLowerCase() === "true" : true,
                events: r.events
                  ? r.events.split(/[;,]/).map((e) => e.trim()).filter(Boolean)
                  : [],
              }),
            onProgress,
          )
        }
      />
    </Page>
  )
}

// ------------------------------------------------------ webhook detail

/// Single-webhook detail + editor: the endpoint, the enabled/signed
/// toggles, the subscribed-events picker and the custom headers, all
/// editable in place (the editing the list used to do in a dialog). There
/// is no single-GET, so the webhook is picked out of the (cached) list.
export function WebhookDetail({ org, server, id }: Scope & { id: number }) {
  const router = useRouter()
  const queryClient = useQueryClient()
  const key = ["webhooks", org, server]
  const webhooks = useQuery({ queryKey: key, queryFn: () => adminApi.webhooks(org, server).list() })
  const webhook = webhooks.data?.webhooks.find((w) => w.id === id)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const [name, setName] = useState("")
  const [url, setUrl] = useState("")
  const [sign, setSign] = useState(true)
  const [events, setEvents] = useState<string[]>([])
  const [eventSearch, setEventSearch] = useState("")
  const [headerRows, setHeaderRows] = useState<{ name: string; value: string }[]>([])
  const [deleteOpen, setDeleteOpen] = useState(false)
  const [testing, setTesting] = useState(false)

  // Seed the editor from the webhook once it arrives (or the id changes).
  useEffect(() => {
    if (!webhook) return
    setName(webhook.name)
    setUrl(webhook.url)
    setSign(webhook.sign)
    setEvents(webhook.events)
    setHeaderRows(
      Object.entries(webhook.headers ?? {}).map(([name, value]) => ({ name, value })),
    )
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [webhook?.id])

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

  const save = useMutation({
    mutationFn: () =>
      adminApi.webhooks(org, server).update(id, {
        name,
        url,
        sign,
        events,
        headers: headersObject(),
      }),
    onSuccess: () => {
      invalidate()
      toast.success("Webhook saved")
    },
    onError: (err) => errorToast(err, "Could not save the webhook"),
  })

  return (
    <Page
      variant="scroll"
      header={
        <PageHeader
          className="mb-0 items-start"
          backHref={`/orgs/${org}/servers/${server}/webhooks`}
          backLabel="Webhooks"
          title={webhook?.name ?? "Webhook"}
          description={
            <span className="flex flex-wrap items-center gap-2">
              {webhook && <StatusPill status={webhook.enabled ? "Active" : "Disabled"} />}
              <code className="text-xs">{webhook?.url ?? "…"}</code>
            </span>
          }
          action={
            <>
              <Button
                variant="outline"
                size="sm"
                onClick={() => save.mutate()}
                disabled={save.isPending || !webhook || !name.trim() || !url.startsWith("http")}
              >
                {save.isPending ? "Saving…" : "Save changes"}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setTesting(true)}
                disabled={!webhook}
              >
                Send test
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setDeleteOpen(true)}
                disabled={!webhook}
              >
                Delete
              </Button>
            </>
          }
        />
      }
    >
      {webhooks.isSuccess && !webhook ? (
        <p className="text-sm text-muted-foreground">This webhook no longer exists.</p>
      ) : !webhook ? (
        <p className="text-sm text-muted-foreground">Loading…</p>
      ) : (
        <div className="space-y-6">
          <section className="grid gap-3">
            <div className="flex items-center justify-between rounded-md border p-3">
              <div>
                <p className="text-sm font-medium">Enabled</p>
                <p className="text-xs text-muted-foreground">Deliver events to this endpoint.</p>
              </div>
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
            </div>
          </section>

          <section className="space-y-4">
            <h2 className="text-sm font-medium">Endpoint</h2>
            <div className="grid gap-2">
              <Label>Name</Label>
              <Input value={name} onChange={(e) => setName(e.target.value)} />
            </div>
            <div className="grid gap-2">
              <Label>URL</Label>
              <div className="flex items-center gap-2">
                <Input
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  placeholder="https://…"
                />
                <CopyButton value={url} />
              </div>
            </div>
            <div className="flex items-center gap-2">
              <Switch checked={sign} onCheckedChange={setSign} id="sign" />
              <Label htmlFor="sign">Sign payloads (RSA)</Label>
            </div>
          </section>

          <section className="space-y-2">
            <h2 className="text-sm font-medium">Subscribed events</h2>
            <p className="text-sm text-muted-foreground">
              {events.length === 0
                ? "Nothing selected, so every event is delivered."
                : "None selected = all events."}
            </p>
            <div className="relative">
              <SearchIcon className="absolute left-2.5 top-1/2 size-4 -translate-y-1/2 text-muted-foreground" />
              <Input
                className="pl-8"
                placeholder="Search events…"
                value={eventSearch}
                onChange={(e) => setEventSearch(e.target.value)}
              />
            </div>
            <div className="grid gap-1 rounded-md border p-2">
              {WEBHOOK_EVENTS.filter((event) =>
                event.toLowerCase().includes(eventSearch.toLowerCase()),
              ).map((event) => (
                <label
                  key={event}
                  className="flex cursor-pointer items-center gap-2 rounded px-1.5 py-1 text-sm hover:bg-muted/60"
                >
                  <input
                    type="checkbox"
                    className="size-4 accent-primary"
                    checked={events.includes(event)}
                    onChange={(e) => toggleEvent(event, e.target.checked)}
                  />
                  <EventPill event={event} />
                  <span className="min-w-0 flex-1 truncate text-xs text-muted-foreground">
                    {WEBHOOK_EVENT_META[event]?.description}
                  </span>
                </label>
              ))}
            </div>
            <WebhookPayloadPreview events={events} />
          </section>

          <section className="space-y-2">
            <h2 className="text-sm font-medium">Custom headers</h2>
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
                  onClick={() => setHeaderRows((rows) => rows.filter((_, i) => i !== index))}
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
          </section>
        </div>
      )}

      {testing && webhook && (
        <SendTestDialog
          org={org}
          server={server}
          webhook={webhook}
          onClose={() => setTesting(false)}
        />
      )}
      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        title="Delete webhook"
        description="No further events will be delivered to this URL."
        onConfirm={async () => {
          try {
            await adminApi.webhooks(org, server).delete(id)
            invalidate()
            router.push(`/orgs/${org}/servers/${server}/webhooks`)
          } catch (err) {
            errorToast(err, "Could not delete the webhook")
          }
        }}
      />
    </Page>
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
  const [exportOpen, setExportOpen] = useState(false)
  const [importOpen, setImportOpen] = useState(false)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })
  const rows: SuppressionWithDate[] = suppressions.data?.suppressions ?? []
  // Maps a suppression's stream_id to its stream so the Scope column can
  // name it (a null stream_id means the suppression is server-wide).
  const streamMap = new Map((suppressions.data?.streams ?? []).map((s) => [s.id, s]))

  const exportColumns: ExportColumn<SuppressionWithDate>[] = [
    { key: "address", label: "Address", accessor: (s) => s.address },
    { key: "type", label: "Type", accessor: (s) => s.type },
    { key: "reason", label: "Reason", accessor: (s) => s.reason ?? "" },
    {
      key: "scope",
      label: "Scope",
      accessor: (s) =>
        s.stream_id == null ? "All streams" : streamMap.get(s.stream_id)?.name ?? `Stream #${s.stream_id}`,
    },
    { key: "date_added", label: "Date added", accessor: (s) => s.created_at ?? "" },
  ]
  const importColumns: ImportColumn[] = [
    { key: "address", label: "Address", example: "user@example.com", required: true },
    { key: "reason", label: "Reason", example: "manual" },
  ]

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

  const columns: ColumnDef<SuppressionWithDate>[] = [
    {
      id: "address",
      header: "Address",
      accessorFn: (s) => s.address,
      cell: ({ row }) => (
        <Link
          href={recipientHref(org, server, row.original.address)}
          className="block max-w-[24rem] truncate font-medium transition-colors group-hover:text-primary hover:underline"
        >
          {row.original.address}
        </Link>
      ),
    },
    {
      id: "type",
      header: "Type",
      accessorFn: (s) => s.type,
      filterFn: (row, _id, value) => row.original.type === value,
      cell: ({ row }) => <Badge variant="outline">{row.original.type}</Badge>,
    },
    {
      id: "scope",
      header: "Scope",
      accessorFn: (s) =>
        s.stream_id == null ? "All streams" : streamMap.get(s.stream_id)?.name ?? "Stream",
      cell: ({ row }) => {
        const sid = row.original.stream_id
        if (sid == null) return <span className="text-muted-foreground">All streams</span>
        const stream = streamMap.get(sid)
        return <Badge variant="outline">{stream ? stream.name : `Stream #${sid}`}</Badge>
      },
    },
    {
      id: "reason",
      header: "Reason",
      enableSorting: false,
      accessorFn: (s) => suppressionReasonText(s),
      cell: ({ row }) => (
        <span className="text-muted-foreground">{suppressionReasonText(row.original)}</span>
      ),
    },
    {
      id: "created_at",
      header: "Date added",
      accessorFn: (s) => s.created_at ?? "",
      cell: ({ row }) => (
        <span className="whitespace-nowrap text-muted-foreground">
          {formatDate(row.original.created_at)}
        </span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <Button variant="outline" size="sm" onClick={() => setReactivating(row.original)}>
          Reactivate
        </Button>
      ),
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Suppressions"
          description="Addresses this server will not deliver to (bounces, complaints, unsubscribes)."
          className="mb-0"
          action={
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
                <UploadIcon className="size-4" /> Import
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setExportOpen(true)}
                disabled={rows.length === 0}
              >
                <DownloadIcon className="size-4" /> Export
              </Button>
              <Button size="sm" onClick={() => setOpen(true)}>
                <PlusIcon className="size-4" /> Suppress address
              </Button>
            </div>
          }
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {suppressions.data?.suppressions.length === 0 ? (
          <EmptyState
            icon={BanIcon}
            title="No suppressions yet"
            description="Hard-bouncing addresses land here automatically; you can also suppress addresses by hand."
            action={{ label: "Suppress address", onClick: () => setOpen(true) }}
          />
        ) : (
          <DataTable
            columns={columns}
            data={rows}
            loading={suppressions.isPending}
            searchKeys={["address"]}
            searchPlaceholder="Search addresses…"
            emptyText="No suppressions match your search."
            initialPageSize={20}
            fillHeight
          />
        )}
      </div>
      <ConfirmDialog
        open={reactivating !== null}
        onOpenChange={(open) => !open && setReactivating(null)}
        title={`Reactivate ${reactivating?.address}?`}
        description="The suppression is removed and this server delivers to the address again. Make sure the underlying problem (bounce, complaint) is resolved."
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
      <DataExportDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        title="Export suppressions"
        filename={`suppressions-${server}`}
        columns={exportColumns}
        rows={rows}
      />
      <DataImportDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        title="Import suppressions"
        templateFilename="suppressions-template"
        columns={importColumns}
        onDone={invalidate}
        onImport={(imported, onProgress) =>
          runRowImport(
            imported,
            (r) =>
              adminApi.suppressions(org, server).create({
                type: "recipient",
                address: r.address,
                ...(r.reason ? { reason: r.reason } : {}),
              }),
            onProgress,
          )
        }
      />
    </Page>
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
  const [exportOpen, setExportOpen] = useState(false)
  const [importOpen, setImportOpen] = useState(false)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })
  const rows = addresses.data?.sender_addresses ?? []

  const exportColumns: ExportColumn<SenderAddress>[] = [
    { key: "email_address", label: "Email address", accessor: (a) => a.email_address },
    { key: "confirmed", label: "Confirmed", accessor: (a) => a.verified },
  ]
  const importColumns: ImportColumn[] = [
    { key: "email_address", label: "Email address", example: "person@partner.example", required: true },
  ]

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

  const columns: ColumnDef<SenderAddress>[] = [
    {
      id: "email",
      header: "Email address",
      accessorFn: (a) => a.email_address,
      cell: ({ row }) => (
        <span className="block max-w-[24rem] truncate font-medium">
          {row.original.email_address}
        </span>
      ),
    },
    {
      id: "status",
      header: "Status",
      enableSorting: false,
      accessorFn: (a) => (a.verified ? "confirmed" : "pending"),
      filterFn: (row, _id, value) =>
        (row.original.verified ? "confirmed" : "pending") === value,
      cell: ({ row }) =>
        row.original.verified ? (
          <StatusPill status="Confirmed" />
        ) : (
          <StatusPill status="Pending" tone="amber" />
        ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => (
        <Button variant="ghost" size="sm" onClick={() => setDeleteId(row.original.id)}>
          Delete
        </Button>
      ),
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Senders"
          description="Individual sender addresses this server may send from once confirmed, without verifying a whole domain."
          className="mb-0"
          action={
            <div className="flex items-center gap-2">
              <Button variant="outline" size="sm" onClick={() => setImportOpen(true)}>
                <UploadIcon className="size-4" /> Import
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => setExportOpen(true)}
                disabled={rows.length === 0}
              >
                <DownloadIcon className="size-4" /> Export
              </Button>
              <Button size="sm" onClick={() => { setIssuedToken(null); setOpen(true) }}>
                <PlusIcon className="size-4" /> Add address
              </Button>
            </div>
          }
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        {addresses.data?.sender_addresses.length === 0 ? (
          <EmptyState
            icon={AtSignIcon}
            title="No sender addresses yet"
            description="Authorize a single address to send from, without domain verification."
            action={{ label: "Add address", onClick: () => { setIssuedToken(null); setOpen(true) } }}
          />
        ) : (
          <DataTable
            columns={columns}
            data={addresses.data?.sender_addresses ?? []}
            loading={addresses.isPending}
            searchKeys={["email_address"]}
            searchPlaceholder="Search addresses…"
            emptyText="No sender addresses match your search."
            initialPageSize={20}
            fillHeight
            filters={[
              {
                columnId: "status",
                label: "Status",
                options: [
                  { label: "Confirmed", value: "confirmed" },
                  { label: "Pending", value: "pending" },
                ],
              },
            ]}
          />
        )}
      </div>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add sender address</DialogTitle>
          </DialogHeader>
          {issuedToken ? (
            <div className="grid gap-2">
              <p className="text-sm text-muted-foreground">
                This instance can&apos;t email the confirmation link. Relay this one-time
                token to the address owner, who confirms at
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
      <DataExportDialog
        open={exportOpen}
        onOpenChange={setExportOpen}
        title="Export senders"
        filename={`senders-${server}`}
        columns={exportColumns}
        rows={rows}
      />
      <DataImportDialog
        open={importOpen}
        onOpenChange={setImportOpen}
        title="Import senders"
        description="Each address gets a confirmation email; it can send only once confirmed."
        templateFilename="senders-template"
        columns={importColumns}
        onDone={invalidate}
        onImport={(imported, onProgress) =>
          runRowImport(
            imported,
            (r) => adminApi.senderAddresses(org, server).create(r.email_address),
            onProgress,
          )
        }
      />
    </Page>
  )
}
