"use client"

// Organization home: servers, members, invitations and (owner) settings
// as tab routes under /orgs/:org.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import Link from "next/link"
import { usePathname, useRouter } from "next/navigation"
import { PlusIcon, ServerIcon, ShieldCheckIcon } from "lucide-react"
import { toast } from "sonner"
import {
  ConfirmDialog,
  EmptyState,
  formatDate,
  PageHeader,
  SecretReveal,
} from "@/components/shared"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
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
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
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

// ------------------------------------------------------------- servers

export function Servers({ org }: { org: string }) {
  const queryClient = useQueryClient()
  const router = useRouter()
  const role = useOrgRole(org)
  const servers = useQuery({
    queryKey: ["servers", org],
    queryFn: () => adminApi.servers(org).list(),
  })
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
    <div>
      <PageHeader
        title="Mail servers"
        action={
          canManage && (
            <Button size="sm" onClick={() => setOpen(true)}>
              <PlusIcon className="size-4" /> New server
            </Button>
          )
        }
      />
      {servers.data?.servers.length === 0 ? (
        <EmptyState>No servers yet.</EmptyState>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {servers.data?.servers.map((server) => (
            <Link key={server.id} href={`/orgs/${org}/servers/${server.permalink}`}>
              <Card className="transition-colors hover:bg-accent/50">
                <CardHeader className="pb-2">
                  <CardTitle className="flex items-center gap-2 text-base">
                    <ServerIcon className="size-4 text-muted-foreground" />
                    {server.name}
                    <Badge
                      variant={server.mode === "Live" ? "default" : "secondary"}
                      className="ml-auto"
                    >
                      {server.mode}
                    </Badge>
                  </CardTitle>
                </CardHeader>
                <CardContent className="text-xs text-muted-foreground">
                  {server.suspended ? (
                    <Badge variant="destructive">suspended</Badge>
                  ) : (
                    `/${server.permalink}`
                  )}
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      )}
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New mail server</DialogTitle>
          </DialogHeader>
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
    </div>
  )
}

// ------------------------------------------------------------- members

export function Members({ org }: { org: string }) {
  const queryClient = useQueryClient()
  const role = useOrgRole(org)
  const members = useQuery({
    queryKey: ["members", org],
    queryFn: () => adminApi.members(org).list(),
  })
  const [addOpen, setAddOpen] = useState(false)
  const [email, setEmail] = useState("")
  const [newRole, setNewRole] = useState<Role>("member")
  const [removeUser, setRemoveUser] = useState<number | null>(null)

  const invalidate = () => queryClient.invalidateQueries({ queryKey: ["members", org] })
  const canManage = role === "root" || role === "admin" || role === "owner"

  const add = useMutation({
    mutationFn: () => adminApi.members(org).add(email, newRole),
    onSuccess: () => {
      invalidate()
      setAddOpen(false)
      setEmail("")
    },
    onError: (err) => errorToast(err, "Could not add the member"),
  })

  return (
    <div>
      <PageHeader
        title="Members"
        description="People with access to this organization."
        action={
          canManage && (
            <Button size="sm" onClick={() => setAddOpen(true)}>
              <PlusIcon className="size-4" /> Add member
            </Button>
          )
        }
      />
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>User</TableHead>
            <TableHead>Email</TableHead>
            <TableHead>Role</TableHead>
            <TableHead>Since</TableHead>
            <TableHead />
          </TableRow>
        </TableHeader>
        <TableBody>
          {members.data?.members.map(({ user, role: memberRole, created_at }) => (
            <TableRow key={user.id}>
              <TableCell>
                {user.first_name} {user.last_name}
                {user.admin && (
                  <Badge variant="secondary" className="ml-2">
                    instance admin
                  </Badge>
                )}
              </TableCell>
              <TableCell className="text-muted-foreground">{user.email_address}</TableCell>
              <TableCell>
                {canManage ? (
                  <Select
                    value={memberRole}
                    onValueChange={async (value) => {
                      try {
                        await adminApi.members(org).setRole(user.id, value as Role)
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
                      {ROLES.map((r) => (
                        <SelectItem key={r} value={r}>
                          {r}
                        </SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                ) : (
                  <Badge variant="outline">{memberRole}</Badge>
                )}
              </TableCell>
              <TableCell className="text-muted-foreground">{formatDate(created_at)}</TableCell>
              <TableCell className="text-right">
                {canManage && (
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setRemoveUser(user.id)}
                  >
                    Remove
                  </Button>
                )}
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>

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
    </div>
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
        <EmptyState>No invitations.</EmptyState>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Email</TableHead>
              <TableHead>Role</TableHead>
              <TableHead>Status</TableHead>
              <TableHead>Expires</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {invitations.data?.invitations.map((invitation) => (
              <TableRow key={invitation.id}>
                <TableCell>{invitation.email_address}</TableCell>
                <TableCell>
                  <Badge variant="outline">{invitation.role}</Badge>
                </TableCell>
                <TableCell>
                  {invitation.accepted_at ? (
                    <Badge>accepted</Badge>
                  ) : new Date(invitation.expires_at) < new Date() ? (
                    <Badge variant="destructive">expired</Badge>
                  ) : (
                    <Badge variant="secondary">pending</Badge>
                  )}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {formatDate(invitation.expires_at)}
                </TableCell>
                <TableCell className="text-right">
                  {canManage && !invitation.accepted_at && (
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={async () => {
                        try {
                          await adminApi.invitations(org).revoke(invitation.id)
                          invalidate()
                        } catch (err) {
                          errorToast(err, "Could not revoke the invitation")
                        }
                      }}
                    >
                      Revoke
                    </Button>
                  )}
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
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

  // Billing needs admin+; security and the danger zone stay owner-only.
  const canBilling = role === "root" || role === "owner" || role === "admin"
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

  // `enabled: false` (the self-hosted default) hides billing entirely.
  const billing = useQuery({
    queryKey: ["billing", org],
    queryFn: () => adminApi.billing(org).get(),
    enabled: canBilling,
  })
  const showBilling = canBilling && billing.data?.enabled === true

  const openPortal = useMutation({
    mutationFn: () => adminApi.billing(org).portal(),
    onSuccess: ({ url }) => {
      window.location.href = url
    },
    onError: (err) =>
      toast.error(
        err instanceof ApiError && err.code === "BillingUnavailable"
          ? "Billing is temporarily unavailable. Please try again in a few minutes."
          : err instanceof ApiError
            ? err.message
            : "Could not open the billing portal",
      ),
  })

  // Admins only see this page when billing is on (owners always do).
  if (!isOwner && !showBilling) {
    return <EmptyState>Only owners can manage organization settings.</EmptyState>
  }
  return (
    <div className="max-w-lg space-y-4">
      {showBilling && (
        <>
          <PageHeader title="Billing" />
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Billing Portal</CardTitle>
            </CardHeader>
            <CardContent className="flex items-center justify-between gap-4">
              <p className="text-sm text-muted-foreground">
                Manage your subscription, payment methods and invoices in the
                billing portal.
              </p>
              <Button
                onClick={() => openPortal.mutate()}
                disabled={openPortal.isPending}
              >
                {openPortal.isPending ? "Opening…" : "Billing Portal"}
              </Button>
            </CardContent>
          </Card>
        </>
      )}
      {isOwner && (
        <>
          <PageHeader title="Security" />
          <Card>
            <CardHeader>
              <CardTitle className="text-base">
                Require two-factor authentication
              </CardTitle>
            </CardHeader>
            <CardContent className="flex items-center justify-between gap-4">
              <p className="text-sm text-muted-foreground">
                Every member must have two-factor authentication (an
                authenticator app or a passkey) to access this organization.
                Members without it are blocked until they enable it — that
                includes you.
              </p>
              <Switch
                checked={orgQuery.data?.organization.require_two_factor ?? false}
                onCheckedChange={(value) => setRequireTwoFactor.mutate(value)}
                disabled={!orgQuery.data || setRequireTwoFactor.isPending}
              />
            </CardContent>
          </Card>
          <PageHeader title="Danger zone" />
          <Card>
            <CardHeader>
              <CardTitle className="text-base">Delete this organization</CardTitle>
            </CardHeader>
            <CardContent className="flex items-center justify-between gap-4">
              <p className="text-sm text-muted-foreground">
                Deletes the organization with all servers and their data.
              </p>
              <Button variant="destructive" onClick={() => setConfirmOpen(true)}>
                Delete
              </Button>
            </CardContent>
          </Card>
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
        </>
      )}
    </div>
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
            back — access is restored immediately.
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
export function OrgShell({ org, children }: { org: string; children: React.ReactNode }) {
  const router = useRouter()
  const pathname = usePathname() ?? ""
  const tab = pathname.split(`/orgs/${org}`)[1]?.split("/")[1] || "servers"

  return (
    <div>
      <PageHeader title={org} description="Organization" />
      <Tabs
        value={tab}
        onValueChange={(value) =>
          router.push(`/orgs/${org}${value === "servers" ? "" : `/${value}`}`)
        }
      >
        <TabsList className="mb-4">
          <TabsTrigger value="servers">Servers</TabsTrigger>
          <TabsTrigger value="members">Members</TabsTrigger>
          <TabsTrigger value="invitations">Invitations</TabsTrigger>
          <TabsTrigger value="settings">Settings</TabsTrigger>
        </TabsList>
      </Tabs>
      {children}
    </div>
  )
}
