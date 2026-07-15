"use client"

// Organization home: servers, members, invitations and (owner) settings
// as tab routes under /orgs/:org.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { type ColumnDef } from "@tanstack/react-table"
import Link from "next/link"
import { useRouter } from "next/navigation"
import {
  MailPlusIcon,
  PlusIcon,
  ShieldCheckIcon,
  UsersIcon,
} from "lucide-react"
import { toast } from "sonner"
import {
  ConfirmDialog,
  EmptyState as SimpleEmptyState,
  formatDate,
  PageHeader,
  SecretReveal,
} from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { FormDialog } from "@/components/form-dialog"
import { OnboardingChecklist } from "@/components/onboarding-checklist"
import { OrgServersTable } from "@/views/dashboard-tables"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { DataTable } from "@/components/ui/data-table"
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Field, FormActions, FormSection, FormSections } from "@/components/form-section"
import { Page } from "@/components/page"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { Switch } from "@/components/ui/switch"
import { adminApi, ApiError, type Role } from "@/lib/api"
import { useAuth } from "@/lib/auth"

const ROLES: Role[] = ["viewer", "member", "admin", "owner"]

function useOrgRole(org: string): Role | "root" | null {
  const { me } = useAuth()
  if (!me) return null
  if (me.user.admin) return "root"
  return me.memberships.find((m) => m.organization.permalink === org)?.role ?? null
}

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

// -------------------------------------------------------------- overview

// The org overview / dashboard: onboarding checklist + 30-day KPI tiles
// aggregated from the admin servers-stats endpoint — no per-server API
// credential required.
export function OrgOverview({ org }: { org: string }) {
  const servers = useQuery({
    queryKey: ["servers", org],
    queryFn: () => adminApi.servers(org).list(),
  })
  const stats = useQuery({
    queryKey: ["server-stats", org],
    queryFn: () => adminApi.servers(org).stats(),
  })

  const agg = (stats.data?.stats ?? []).reduce(
    (a, s) => ({
      outgoing: a.outgoing + s.outgoing,
      incoming: a.incoming + s.incoming,
      bounced: a.bounced + s.bounced,
      total: a.total + s.total,
    }),
    { outgoing: 0, incoming: 0, bounced: 0, total: 0 },
  )
  const num = (n: number) => n.toLocaleString("en-US")
  const bounceRate =
    agg.total > 0 ? `${((agg.bounced / agg.total) * 100).toFixed(1)}%` : "–"

  const tiles = [
    {
      label: "Servers",
      value: servers.data ? num(servers.data.servers.length) : "–",
      hint: "Mail servers in this organization",
    },
    { label: "Outgoing (30d)", value: num(agg.outgoing), hint: "Messages sent across all servers" },
    {
      label: "Inbound (30d)",
      value: num(agg.incoming),
      hint: "Messages received across all servers",
    },
    { label: "Bounce rate (30d)", value: bounceRate, hint: "Bounced share of all messages" },
  ]

  return (
    <Page
      variant="scroll"
      header={
        <PageHeader
          title="Dashboard"
          description="Mail activity across this organization's servers over the last 30 days."
          className="mb-0"
        />
      }
    >
      <OnboardingChecklist org={org} />
      <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4 *:data-[slot=card]:from-primary/5 *:data-[slot=card]:to-card *:data-[slot=card]:bg-gradient-to-t *:data-[slot=card]:shadow-xs">
        {tiles.map((t) => (
          <Card key={t.label} className="@container/card gap-2 py-5">
            <CardHeader>
              <CardDescription>{t.label}</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums @[180px]/card:text-3xl">
                {t.value}
              </CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">{t.hint}</CardContent>
          </Card>
        ))}
      </div>
    </Page>
  )
}

// ------------------------------------------------------------- servers

// The org-level Servers page: a servers data table + create dialog.
export function Servers({ org }: { org: string }) {
  const queryClient = useQueryClient()
  const router = useRouter()
  const role = useOrgRole(org)
  const [open, setOpen] = useState(false)
  const [name, setName] = useState("")
  const [mode, setMode] = useState("Live")

  const create = useMutation({
    mutationFn: () => adminApi.servers(org).create(name, mode),
    onSuccess: ({ server }) => {
      queryClient.invalidateQueries({ queryKey: ["servers", org] })
      setOpen(false)
      router.push(`/orgs/${org}/servers/${server.permalink}`)
    },
    onError: (err) => errorToast(err, "Could not create the server"),
  })

  const canManage = role === "root" || role === "admin" || role === "owner"

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Servers"
          description="Mail servers in this organization, with 30-day activity."
          action={
            canManage && (
              <Button size="sm" onClick={() => setOpen(true)}>
                <PlusIcon className="size-4" /> New server
              </Button>
            )
          }
          className="mb-0"
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        <OrgServersTable org={org} fillHeight />
      </div>
      <FormDialog
        open={open}
        onOpenChange={setOpen}
        title="New mail server"
        onSubmit={() => create.mutate()}
        busy={create.isPending}
        submitDisabled={!name.trim()}
      >
        <div className="grid gap-4">
          <div className="grid gap-2">
            <Label>Name</Label>
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Production" />
          </div>
          <div className="grid gap-2">
            <Label>Mode</Label>
            <Select value={mode} onValueChange={setMode}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="Live">Live</SelectItem>
                <SelectItem value="Development">Development</SelectItem>
              </SelectContent>
            </Select>
          </div>
        </div>
      </FormDialog>
    </Page>
  )
}

// ------------------------------------------------------------- members

type MemberRow =
  | {
      kind: "member"
      key: string
      userId: number
      name: string
      email: string
      role: Role
      isAdmin: boolean
      since: string
    }
  | {
      kind: "invite"
      key: string
      inviteId: number
      name: string
      email: string
      role: string
      since: string
      state: "pending" | "expired"
    }

export function Members({ org }: { org: string }) {
  const queryClient = useQueryClient()
  const role = useOrgRole(org)
  const canManage = role === "root" || role === "admin" || role === "owner"

  const members = useQuery({
    queryKey: ["members", org],
    queryFn: () => adminApi.members(org).list(),
  })
  const invitations = useQuery({
    queryKey: ["invitations", org],
    queryFn: () => adminApi.invitations(org).list(),
  })

  const [addOpen, setAddOpen] = useState(false)
  const [inviteOpen, setInviteOpen] = useState(false)
  const [email, setEmail] = useState("")
  const [newRole, setNewRole] = useState<Role>("member")
  const [issued, setIssued] = useState<{ token: string; url?: string } | null>(null)
  const [removeUser, setRemoveUser] = useState<number | null>(null)

  const invalidate = () => {
    queryClient.invalidateQueries({ queryKey: ["members", org] })
    queryClient.invalidateQueries({ queryKey: ["invitations", org] })
  }

  const add = useMutation({
    mutationFn: () => adminApi.members(org).add(email, newRole),
    onSuccess: () => {
      invalidate()
      setAddOpen(false)
      setEmail("")
    },
    onError: (err) => errorToast(err, "Could not add the member"),
  })

  const invite = useMutation({
    mutationFn: () => adminApi.invitations(org).create(email, newRole),
    onSuccess: ({ invitation }) => {
      invalidate()
      setEmail("")
      setIssued({
        token: invitation.invite_token!,
        url:
          invitation.invite_url ??
          `${window.location.origin}/invitations/accept?token=${invitation.invite_token}`,
      })
    },
    onError: (err) => errorToast(err, "Could not create the invitation"),
  })

  // Members and their pending invitations, in one table.
  const rows: MemberRow[] = [
    ...(members.data?.members ?? []).map((m) => ({
      kind: "member" as const,
      key: `m${m.user.id}`,
      userId: m.user.id,
      name:
        [m.user.first_name, m.user.last_name].filter(Boolean).join(" ") ||
        m.user.email_address,
      email: m.user.email_address,
      role: m.role as Role,
      isAdmin: m.user.admin,
      since: m.created_at,
    })),
    ...(invitations.data?.invitations ?? [])
      .filter((i) => !i.accepted_at)
      .map((i) => ({
        kind: "invite" as const,
        key: `i${i.id}`,
        inviteId: i.id,
        name: i.email_address,
        email: i.email_address,
        role: i.role,
        since: i.expires_at,
        state: (new Date(i.expires_at) < new Date() ? "expired" : "pending") as
          | "expired"
          | "pending",
      })),
  ]

  const columns: ColumnDef<MemberRow>[] = [
    {
      id: "member",
      header: "Member",
      accessorFn: (r) => r.name,
      cell: ({ row }) => {
        const r = row.original
        return (
          <div className="flex min-w-0 flex-col">
            <span className="flex items-center gap-2 font-medium">
              <span className="truncate">{r.name}</span>
              {r.kind === "member" && r.isAdmin && (
                <Badge variant="secondary" className="shrink-0">
                  instance admin
                </Badge>
              )}
            </span>
            <span className="truncate text-xs text-muted-foreground">
              {r.kind === "invite" ? "Invitation" : r.email}
            </span>
          </div>
        )
      },
    },
    {
      id: "role",
      header: "Role",
      accessorFn: (r) => r.role,
      cell: ({ row }) => {
        const r = row.original
        if (r.kind === "member" && canManage) {
          return (
            <Select
              value={r.role}
              onValueChange={async (value) => {
                try {
                  await adminApi.members(org).setRole(r.userId, value as Role)
                  invalidate()
                } catch (err) {
                  errorToast(err, "Could not change the role")
                }
              }}
            >
              <SelectTrigger size="sm" className="w-28">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {ROLES.map((x) => (
                  <SelectItem key={x} value={x}>
                    {x}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )
        }
        return (
          <Badge variant="outline" className="capitalize">
            {r.role}
          </Badge>
        )
      },
    },
    {
      id: "status",
      header: "Status",
      enableSorting: false,
      accessorFn: (r) => (r.kind === "member" ? "Active" : r.state),
      cell: ({ row }) => {
        const r = row.original
        if (r.kind === "member")
          return (
            <Badge
              variant="outline"
              className="border-emerald-300 bg-emerald-50 font-medium text-emerald-700"
            >
              Active
            </Badge>
          )
        if (r.state === "expired") return <Badge variant="destructive">Expired</Badge>
        return (
          <Badge
            variant="outline"
            className="border-amber-300 bg-amber-50 font-medium text-amber-700"
          >
            Invited
          </Badge>
        )
      },
    },
    {
      id: "since",
      header: "Since",
      accessorFn: (r) => r.since,
      cell: ({ row }) => (
        <span className="text-muted-foreground">{formatDate(row.original.since)}</span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) => {
        if (!canManage) return null
        const r = row.original
        return r.kind === "member" ? (
          <Button variant="ghost" size="sm" onClick={() => setRemoveUser(r.userId)}>
            Remove
          </Button>
        ) : (
          <Button
            variant="ghost"
            size="sm"
            onClick={async () => {
              try {
                await adminApi.invitations(org).revoke(r.inviteId)
                invalidate()
              } catch (err) {
                errorToast(err, "Could not revoke the invitation")
              }
            }}
          >
            Revoke
          </Button>
        )
      },
    },
  ]

  return (
    <Page
      variant="fill"
      header={
        <PageHeader
          title="Members"
          description="People with access to this organization, including pending invitations."
          action={
            canManage && (
              <div className="flex gap-2">
                <Button
                  size="sm"
                  variant="outline"
                  onClick={() => {
                    setIssued(null)
                    setEmail("")
                    setInviteOpen(true)
                  }}
                >
                  <MailPlusIcon className="size-4" /> Invite
                </Button>
                <Button size="sm" onClick={() => setAddOpen(true)}>
                  <PlusIcon className="size-4" /> Add member
                </Button>
              </div>
            )
          }
          className="mb-0"
        />
      }
    >
      <div className="flex min-h-0 flex-1 flex-col">
        <DataTable
          columns={columns}
          data={rows}
          loading={members.isPending}
          searchKeys={["name", "email"]}
          searchPlaceholder="Search members…"
          emptyText="No members yet."
          fillHeight
        />
      </div>

      {/* Add an existing account. */}
      <Dialog open={addOpen} onOpenChange={setAddOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add an existing account</DialogTitle>
          </DialogHeader>
          <div className="grid gap-4">
            <div className="grid gap-2">
              <Label>Email address</Label>
              <Input value={email} onChange={(e) => setEmail(e.target.value)} />
              <p className="text-xs text-muted-foreground">
                For people without an account, send an invitation instead.
              </p>
            </div>
            <div className="grid gap-2">
              <Label>Role</Label>
              <Select value={newRole} onValueChange={(value) => setNewRole(value as Role)}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {ROLES.map((r) => (
                    <SelectItem key={r} value={r}>
                      {r}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setAddOpen(false)}>
              Cancel
            </Button>
            <Button onClick={() => add.mutate()} disabled={add.isPending || !email.includes("@")}>
              Add
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      {/* Invite by email — the link is shown once after creating. */}
      <Dialog open={inviteOpen} onOpenChange={setInviteOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Invite to the organization</DialogTitle>
          </DialogHeader>
          {issued ? (
            <SecretReveal label="Invitation link" value={issued.url ?? issued.token} />
          ) : (
            <div className="grid gap-4">
              <div className="grid gap-2">
                <Label>Email address</Label>
                <Input value={email} onChange={(e) => setEmail(e.target.value)} />
              </div>
              <div className="grid gap-2">
                <Label>Role</Label>
                <Select value={newRole} onValueChange={(value) => setNewRole(value as Role)}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {ROLES.map((r) => (
                      <SelectItem key={r} value={r}>
                        {r}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setInviteOpen(false)}>
              {issued ? "Done" : "Cancel"}
            </Button>
            {!issued && (
              <Button
                onClick={() => invite.mutate()}
                disabled={invite.isPending || !email.includes("@")}
              >
                Create invitation
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={removeUser !== null}
        onOpenChange={(open) => !open && setRemoveUser(null)}
        title="Remove member"
        description="The user keeps their account but loses access to this organization."
        confirmLabel="Remove"
        onConfirm={async () => {
          try {
            await adminApi.members(org).remove(removeUser!)
            invalidate()
          } catch (err) {
            errorToast(err, "Could not remove the member")
          }
        }}
      />
    </Page>
  )
}

// --------------------------------------------------------- invitations

export function Invitations({ org }: { org: string }) {
  const queryClient = useQueryClient()
  const role = useOrgRole(org)
  const invitations = useQuery({
    queryKey: ["invitations", org],
    queryFn: () => adminApi.invitations(org).list(),
  })
  const [open, setOpen] = useState(false)
  const [email, setEmail] = useState("")
  const [newRole, setNewRole] = useState<Role>("member")
  const [issued, setIssued] = useState<{ token: string; url?: string } | null>(null)

  const canManage = role === "root" || role === "admin" || role === "owner"
  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["invitations", org] })

  const create = useMutation({
    mutationFn: () => adminApi.invitations(org).create(email, newRole),
    onSuccess: ({ invitation }) => {
      invalidate()
      setEmail("")
      setIssued({
        token: invitation.invite_token!,
        url:
          invitation.invite_url ??
          `${window.location.origin}/invitations/accept?token=${invitation.invite_token}`,
      })
    },
    onError: (err) => errorToast(err, "Could not create the invitation"),
  })

  type Invite = NonNullable<typeof invitations.data>["invitations"][number]
  const inviteColumns: ColumnDef<Invite>[] = [
    {
      id: "email",
      header: "Email",
      accessorFn: (r) => r.email_address,
      cell: ({ row }) => (
        <span className="block truncate font-medium">{row.original.email_address}</span>
      ),
    },
    {
      id: "role",
      header: "Role",
      accessorFn: (r) => r.role,
      cell: ({ row }) => (
        <Badge variant="outline" className="capitalize">
          {row.original.role}
        </Badge>
      ),
    },
    {
      id: "status",
      header: "Status",
      enableSorting: false,
      accessorFn: (r) =>
        r.accepted_at ? "accepted" : new Date(r.expires_at) < new Date() ? "expired" : "pending",
      cell: ({ row }) => {
        const i = row.original
        if (i.accepted_at)
          return (
            <Badge
              variant="outline"
              className="border-emerald-300 bg-emerald-50 font-medium text-emerald-700"
            >
              Accepted
            </Badge>
          )
        if (new Date(i.expires_at) < new Date()) return <Badge variant="destructive">Expired</Badge>
        return (
          <Badge
            variant="outline"
            className="border-amber-300 bg-amber-50 font-medium text-amber-700"
          >
            Pending
          </Badge>
        )
      },
    },
    {
      id: "expires",
      header: "Expires",
      accessorFn: (r) => r.expires_at,
      cell: ({ row }) => (
        <span className="text-muted-foreground">{formatDate(row.original.expires_at)}</span>
      ),
    },
    {
      id: "actions",
      header: "",
      enableSorting: false,
      meta: { align: "right" },
      cell: ({ row }) =>
        canManage && !row.original.accepted_at ? (
          <Button
            variant="ghost"
            size="sm"
            onClick={async () => {
              try {
                await adminApi.invitations(org).revoke(row.original.id)
                invalidate()
              } catch (err) {
                errorToast(err, "Could not revoke the invitation")
              }
            }}
          >
            Revoke
          </Button>
        ) : null,
    },
  ]

  return (
    <div>
      <PageHeader
        title="Invitations"
        description="Invite people by email; the link is shown once after creating."
        action={
          canManage && (
            <Button size="sm" onClick={() => { setIssued(null); setOpen(true) }}>
              <PlusIcon className="size-4" /> Invite
            </Button>
          )
        }
      />
      {invitations.data?.invitations.length === 0 ? (
        <EmptyState
          icon={MailPlusIcon}
          title="No invitations yet"
          description="Invite teammates by email. Each one gets a one-time link to join this organization."
          action={
            canManage
              ? { label: "Invite", onClick: () => { setIssued(null); setOpen(true) } }
              : undefined
          }
        />
      ) : (
        <DataTable
          columns={inviteColumns}
          data={invitations.data?.invitations ?? []}
          loading={invitations.isPending}
          searchKeys={["email_address"]}
          searchPlaceholder="Search invitations…"
          emptyText="No invitations match."
        />
      )}

      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Invite to the organization</DialogTitle>
          </DialogHeader>
          {issued ? (
            <SecretReveal label="Invitation link" value={issued.url ?? issued.token} />
          ) : (
            <div className="grid gap-4">
              <div className="grid gap-2">
                <Label>Email address</Label>
                <Input value={email} onChange={(e) => setEmail(e.target.value)} />
              </div>
              <div className="grid gap-2">
                <Label>Role</Label>
                <Select value={newRole} onValueChange={(value) => setNewRole(value as Role)}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {ROLES.map((r) => (
                      <SelectItem key={r} value={r}>
                        {r}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
          )}
          <DialogFooter>
            <Button variant="outline" onClick={() => setOpen(false)}>
              {issued ? "Done" : "Cancel"}
            </Button>
            {!issued && (
              <Button
                onClick={() => create.mutate()}
                disabled={create.isPending || !email.includes("@")}
              >
                Create invitation
              </Button>
            )}
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  )
}

// ------------------------------------------------------------ settings

export function OrgSettings({ org }: { org: string }) {
  const router = useRouter()
  const queryClient = useQueryClient()
  const { refresh } = useAuth()
  const role = useOrgRole(org)
  const [confirmOpen, setConfirmOpen] = useState(false)

  // Security and the danger zone are owner-only; billing moved to its own
  // Usage & Billing tab (views/org/Billing.tsx).
  const isOwner = role === "root" || role === "owner"

  const orgQuery = useQuery({
    queryKey: ["org", org],
    queryFn: () => adminApi.organizations.get(org),
    enabled: isOwner,
    retry: false,
  })
  const setRequireTwoFactor = useMutation({
    mutationFn: (require_two_factor: boolean) =>
      adminApi.organizations.update(org, { require_two_factor }),
    onSuccess: ({ organization }) => {
      queryClient.setQueryData(["org", org], { organization })
      toast.success(
        organization.require_two_factor
          ? "Two-factor authentication is now required for this organization."
          : "Two-factor authentication is no longer required.",
      )
    },
    onError: (err) => errorToast(err, "Could not update the organization"),
  })

  if (!isOwner) {
    return <SimpleEmptyState>Only owners can manage organization settings.</SimpleEmptyState>
  }
  return (
    <Page
      header={
        <PageHeader
          title="Settings"
          description="Security and configuration for this organization."
          className="mb-0"
        />
      }
    >
      <FormSections>
        <FormSection
          title="Security"
          description="Access requirements for everyone in this organization."
        >
          <Field
            label="Require two-factor authentication"
            span={6}
            hint="Every member must have two-factor authentication (an authenticator app or a passkey) to access this organization. Members without it are blocked until they enable it, and that includes you."
          >
            <div className="flex items-center gap-2">
              <Switch
                id="require-2fa"
                checked={orgQuery.data?.organization.require_two_factor ?? false}
                onCheckedChange={(value) => setRequireTwoFactor.mutate(value)}
                disabled={!orgQuery.data || setRequireTwoFactor.isPending}
              />
              <Label htmlFor="require-2fa">
                {orgQuery.data?.organization.require_two_factor ? "Required" : "Optional"}
              </Label>
            </div>
          </Field>
        </FormSection>

        <FormSection
          title="Danger zone"
          description="Deletes the organization with all servers and their data. This cannot be undone."
        >
          <FormActions>
            <Button variant="destructive" onClick={() => setConfirmOpen(true)}>
              Delete organization
            </Button>
          </FormActions>
        </FormSection>
      </FormSections>

      <ConfirmDialog
        open={confirmOpen}
        onOpenChange={setConfirmOpen}
        title={`Delete ${org}?`}
        description="This cannot be undone. All servers, domains, credentials and messages are removed."
        onConfirm={async () => {
          try {
            await adminApi.organizations.delete(org)
            await refresh()
            router.push("/")
          } catch (err) {
            errorToast(err, "Could not delete the organization")
          }
        }}
      />
    </Page>
  )
}

// ------------------------------------------------------------ 2FA gate

/// Wraps every /orgs/[org] page: when the API answers `TwoFactorEnforced`
/// (the organization requires 2FA and this account has none), a friendly
/// full-page card replaces the organization content.
export function OrgTwoFactorGate({
  org,
  children,
}: {
  org: string
  children: React.ReactNode
}) {
  const orgQuery = useQuery({
    queryKey: ["org", org],
    queryFn: () => adminApi.organizations.get(org),
    retry: false,
  })
  const enforced =
    orgQuery.error instanceof ApiError &&
    orgQuery.error.code === "TwoFactorEnforced"

  if (!enforced) return <>{children}</>
  return (
    <div className="flex min-h-[60vh] items-center justify-center">
      <Card className="max-w-md text-center">
        <CardHeader>
          <ShieldCheckIcon className="mx-auto size-10 text-muted-foreground" />
          <CardTitle className="text-lg">
            Enable 2FA to access this organization
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <p className="text-sm text-muted-foreground">
            This organization requires two-factor authentication. Set up an
            authenticator app or a passkey on your account and come right
            back. Access is restored immediately.
          </p>
          <Button asChild>
            <Link href="/account">Go to Account &amp; Security</Link>
          </Button>
        </CardContent>
      </Card>
    </div>
  )
}

// ---------------------------------------------------------------- shell

/// Header + tab bar of /orgs/[org]; pages render below as children.
export function OrgShell({ children }: { org: string; children: React.ReactNode }) {
  // Org-level navigation lives in the sidebar and each sub-page renders its
  // own header, so this shell is now just a passthrough.
  return <div>{children}</div>
}
