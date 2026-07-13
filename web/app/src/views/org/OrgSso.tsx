"use client"

// Tenant single sign-on (/orgs/[org]/sso, admin+): the email domains that
// route logins to this organization, and its OIDC / SAML / social
// connections. A signed-out user who enters an address under a verified
// domain is offered these connections on the login page.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { FingerprintIcon, GlobeIcon, PlusIcon } from "lucide-react"
import { toast } from "sonner"
import { ConfirmDialog, CopyButton, PageHeader } from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { FormDialog } from "@/components/form-dialog"
import { StatusPill } from "@/components/status-pill"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
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
import { Textarea } from "@/components/ui/textarea"
import {
  adminApi,
  ApiError,
  type OrgSsoConnection,
  type OrgSsoKind,
  type Role,
} from "@/lib/api"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

const KINDS: { value: OrgSsoKind; label: string }[] = [
  { value: "oidc", label: "OpenID Connect" },
  { value: "saml", label: "SAML 2.0" },
  { value: "google", label: "Google" },
  { value: "microsoft", label: "Microsoft" },
  { value: "github", label: "GitHub" },
]

function kindLabel(kind: OrgSsoKind): string {
  return KINDS.find((k) => k.value === kind)?.label ?? kind
}

/// Which config fields a connection kind needs. `secret` fields render as
/// password inputs and keep the stored value when left at the mask.
type Field = {
  key: string
  label: string
  placeholder?: string
  secret?: boolean
  multiline?: boolean
  optional?: boolean
}

function fieldsFor(kind: OrgSsoKind): Field[] {
  switch (kind) {
    case "oidc":
      return [
        { key: "issuer", label: "Issuer URL", placeholder: "https://login.example.com" },
        { key: "client_id", label: "Client ID" },
        { key: "client_secret", label: "Client secret", secret: true },
      ]
    case "saml":
      return [
        {
          key: "idp_sso_url",
          label: "IdP single sign-on URL",
          placeholder: "https://idp.example.com/sso",
        },
        {
          key: "idp_certificate",
          label: "IdP signing certificate (PEM)",
          placeholder: "-----BEGIN CERTIFICATE-----",
          multiline: true,
        },
        { key: "sp_entity_id", label: "SP entity ID (optional)", optional: true },
      ]
    case "microsoft":
      return [
        { key: "client_id", label: "Client ID" },
        { key: "client_secret", label: "Client secret", secret: true },
        {
          key: "issuer",
          label: "Issuer (optional, for single-tenant apps)",
          placeholder: "https://login.microsoftonline.com/{tenant}/v2.0",
          optional: true,
        },
      ]
    default:
      return [
        { key: "client_id", label: "Client ID" },
        { key: "client_secret", label: "Client secret", secret: true },
      ]
  }
}

// ------------------------------------------------------------- domains

function EmailDomains({ org }: { org: string }) {
  const queryClient = useQueryClient()
  const key = ["org-sso-domains", org]
  const domains = useQuery({ queryKey: key, queryFn: () => adminApi.orgSso(org).domains.list() })
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const create = useMutation({
    mutationFn: () => adminApi.orgSso(org).domains.create(name),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      setName("")
    },
    onError: (err) => errorToast(err, "Could not add the domain"),
  })

  const rows = domains.data?.domains ?? []

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between space-y-0">
        <div>
          <CardTitle className="text-base">Login email domains</CardTitle>
          <CardDescription>
            Team members whose email address belongs to a verified domain are routed to this
            organization&apos;s sign-in.
          </CardDescription>
        </div>
        <Button size="sm" onClick={() => setOpen(true)}>
          <PlusIcon className="size-4" /> Add domain
        </Button>
      </CardHeader>
      <CardContent>
        {domains.isSuccess && rows.length === 0 ? (
          <EmptyState
            icon={GlobeIcon}
            title="No login domains yet"
            description="Verify a domain like acme.com so your team's sign-ins route to this organization."
            action={{ label: "Add domain", onClick: () => setOpen(true) }}
          />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Domain</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>DNS record</TableHead>
                <TableHead />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((domain) => (
                <TableRow key={domain.id}>
                  <TableCell className="font-medium">{domain.domain}</TableCell>
                  <TableCell>
                    <StatusPill status={domain.verified ? "verified" : "unverified"} />
                  </TableCell>
                  <TableCell>
                    {domain.verified ? (
                      <span className="text-muted-foreground">—</span>
                    ) : (
                      <div className="grid gap-1 text-xs">
                        <span className="inline-flex items-center gap-1">
                          <code className="rounded bg-muted px-1.5 py-0.5 font-mono">
                            {domain.dns_record.name}
                          </code>
                          <CopyButton value={domain.dns_record.name} />
                        </span>
                        <span className="inline-flex items-center gap-1">
                          <code className="max-w-72 truncate rounded bg-muted px-1.5 py-0.5 font-mono">
                            {domain.dns_record.value}
                          </code>
                          <CopyButton value={domain.dns_record.value} />
                        </span>
                      </div>
                    )}
                  </TableCell>
                  <TableCell className="space-x-2 text-right">
                    {!domain.verified && (
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={async () => {
                          try {
                            await adminApi.orgSso(org).domains.verify(domain.id)
                            invalidate()
                            toast.success(`${domain.domain} verified`)
                          } catch (err) {
                            errorToast(err, "Verification failed")
                          }
                        }}
                      >
                        Verify
                      </Button>
                    )}
                    <Button variant="ghost" size="sm" onClick={() => setDeleteId(domain.id)}>
                      Delete
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title="Add login domain"
        submitLabel="Add"
        onSubmit={() => create.mutate()}
        busy={create.isPending}
        submitDisabled={!name.includes(".")}
      >
        <div className="grid gap-2">
          <Label>Email domain</Label>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="acme.com" />
          <p className="text-xs text-muted-foreground">
            After adding, publish the TXT record shown in the table to prove ownership, then
            verify.
          </p>
        </div>
      </FormDialog>
      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Remove this login domain?"
        description="Sign-ins from this domain will no longer route to the organization."
        onConfirm={async () => {
          try {
            await adminApi.orgSso(org).domains.delete(deleteId!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not remove the domain")
          }
        }}
      />
    </Card>
  )
}

// ----------------------------------------------------------- connections

/// The create/edit form state: kind, name, flat config values, role, flag.
type ConnectionForm = {
  kind: OrgSsoKind
  name: string
  config: Record<string, string>
  default_role: Role
  auto_provision: boolean
}

const EMPTY_FORM: ConnectionForm = {
  kind: "oidc",
  name: "",
  config: {},
  default_role: "member",
  auto_provision: true,
}

function ConnectionFields({
  form,
  setForm,
  editing,
}: {
  form: ConnectionForm
  setForm: (form: ConnectionForm) => void
  editing: boolean
}) {
  const setConfig = (key: string, value: string) =>
    setForm({ ...form, config: { ...form.config, [key]: value } })
  return (
    <div className="grid gap-4">
      <div className="grid grid-cols-2 gap-2">
        <div className="grid gap-2">
          <Label>Type</Label>
          <Select
            value={form.kind}
            onValueChange={(value) =>
              setForm({ ...form, kind: value as OrgSsoKind, config: {} })
            }
            disabled={editing}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {KINDS.map((kind) => (
                <SelectItem key={kind.value} value={kind.value}>
                  {kind.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="grid gap-2">
          <Label>Name</Label>
          <Input
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            placeholder="Acme Okta"
          />
        </div>
      </div>
      {fieldsFor(form.kind).map((field) => (
        <div key={field.key} className="grid gap-2">
          <Label>{field.label}</Label>
          {field.multiline ? (
            <Textarea
              value={form.config[field.key] ?? ""}
              onChange={(e) => setConfig(field.key, e.target.value)}
              placeholder={field.placeholder}
              rows={5}
              className="font-mono text-xs"
            />
          ) : (
            <Input
              type={field.secret ? "password" : "text"}
              value={form.config[field.key] ?? ""}
              onChange={(e) => setConfig(field.key, e.target.value)}
              placeholder={field.placeholder}
            />
          )}
          {field.secret && editing && (
            <p className="text-xs text-muted-foreground">
              Leave the masked value untouched to keep the stored secret.
            </p>
          )}
        </div>
      ))}
      <div className="grid grid-cols-2 items-end gap-2">
        <div className="grid gap-2">
          <Label>Role for new members</Label>
          <Select
            value={form.default_role}
            onValueChange={(value) => setForm({ ...form, default_role: value as Role })}
          >
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {(["viewer", "member", "admin"] as Role[]).map((role) => (
                <SelectItem key={role} value={role}>
                  {role}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="flex items-center gap-2 pb-2">
          <Switch
            checked={form.auto_provision}
            onCheckedChange={(checked) => setForm({ ...form, auto_provision: checked })}
            id="auto-provision"
          />
          <Label htmlFor="auto-provision">Create accounts on first sign-in</Label>
        </div>
      </div>
    </div>
  )
}

function Connections({ org }: { org: string }) {
  const queryClient = useQueryClient()
  const key = ["org-sso-connections", org]
  const connections = useQuery({
    queryKey: key,
    queryFn: () => adminApi.orgSso(org).connections.list(),
  })
  const [open, setOpen] = useState(false)
  const [editing, setEditing] = useState<OrgSsoConnection | null>(null)
  const [form, setForm] = useState<ConnectionForm>(EMPTY_FORM)
  const [deleteId, setDeleteId] = useState<number | null>(null)
  const invalidate = () => queryClient.invalidateQueries({ queryKey: key })

  const openCreate = () => {
    setEditing(null)
    setForm(EMPTY_FORM)
    setOpen(true)
  }
  const openEdit = (connection: OrgSsoConnection) => {
    setEditing(connection)
    setForm({
      kind: connection.kind,
      name: connection.name,
      config: { ...connection.config },
      default_role: connection.default_role,
      auto_provision: connection.auto_provision,
    })
    setOpen(true)
  }

  const save = useMutation({
    mutationFn: () =>
      editing
        ? adminApi.orgSso(org).connections.update(editing.id, {
            name: form.name,
            config: form.config,
            default_role: form.default_role,
            auto_provision: form.auto_provision,
          })
        : adminApi.orgSso(org).connections.create({
            kind: form.kind,
            name: form.name,
            config: form.config,
            default_role: form.default_role,
            auto_provision: form.auto_provision,
          }),
    onSuccess: () => {
      invalidate()
      setOpen(false)
      toast.success(editing ? "Connection updated" : "Connection created")
    },
    onError: (err) => errorToast(err, "Could not save the connection"),
  })

  const rows = connections.data?.connections ?? []

  return (
    <Card>
      <CardHeader className="flex flex-row items-start justify-between space-y-0">
        <div>
          <CardTitle className="text-base">Connections</CardTitle>
          <CardDescription>
            The identity providers this organization signs in with. A connection needs a
            verified login domain before it can authenticate anyone.
          </CardDescription>
        </div>
        <Button size="sm" onClick={openCreate}>
          <PlusIcon className="size-4" /> New connection
        </Button>
      </CardHeader>
      <CardContent>
        {connections.isSuccess && rows.length === 0 ? (
          <EmptyState
            icon={FingerprintIcon}
            title="No SSO connections yet"
            description="Connect your identity provider (Okta, Entra ID, Google Workspace, GitHub) so your team signs in with one click."
            action={{ label: "New connection", onClick: openCreate }}
          />
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead>Type</TableHead>
                <TableHead>New members</TableHead>
                <TableHead>Enabled</TableHead>
                <TableHead />
              </TableRow>
            </TableHeader>
            <TableBody>
              {rows.map((connection) => (
                <TableRow key={connection.id}>
                  <TableCell className="font-medium">{connection.name}</TableCell>
                  <TableCell>
                    <Badge variant="outline">{kindLabel(connection.kind)}</Badge>
                  </TableCell>
                  <TableCell className="text-muted-foreground">
                    {connection.auto_provision
                      ? `join as ${connection.default_role}`
                      : "existing members only"}
                  </TableCell>
                  <TableCell>
                    <Switch
                      checked={connection.enabled}
                      onCheckedChange={async (checked) => {
                        try {
                          await adminApi
                            .orgSso(org)
                            .connections.update(connection.id, { enabled: checked })
                          invalidate()
                        } catch (err) {
                          errorToast(err, "Could not toggle the connection")
                        }
                      }}
                    />
                  </TableCell>
                  <TableCell className="space-x-2 text-right">
                    <Button variant="outline" size="sm" onClick={() => openEdit(connection)}>
                      Edit
                    </Button>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setDeleteId(connection.id)}
                    >
                      Delete
                    </Button>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </CardContent>
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title={editing ? `Edit ${editing.name}` : "New SSO connection"}
        submitLabel={editing ? "Save" : "Create"}
        onSubmit={() => save.mutate()}
        busy={save.isPending}
        submitDisabled={!form.name.trim()}
      >
        <ConnectionFields form={form} setForm={setForm} editing={editing !== null} />
      </FormDialog>
      <ConfirmDialog
        open={deleteId !== null}
        onOpenChange={(open) => !open && setDeleteId(null)}
        title="Delete this connection?"
        description="Sign-ins through this identity provider stop working immediately."
        onConfirm={async () => {
          try {
            await adminApi.orgSso(org).connections.delete(deleteId!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not delete the connection")
          }
        }}
      />
    </Card>
  )
}

export default function OrgSso({ org }: { org: string }) {
  return (
    <div className="space-y-6">
      <PageHeader
        title="Single sign-on"
        description="Let your team sign in through your identity provider. Verify a login domain, then connect OIDC, SAML or a social provider."
      />
      <EmailDomains org={org} />
      <Connections org={org} />
    </div>
  )
}
