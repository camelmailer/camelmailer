"use client"

// Mail-server home: settings + resource tabs + messaging, routed under
// /orgs/:org/servers/:server.

import { useState } from "react"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { usePathname, useRouter } from "next/navigation"
import { toast } from "sonner"
import { ConfirmDialog, PageHeader } from "@/components/shared"
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
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import { adminApi, ApiError, type Server } from "@/lib/api"
import { useAuth } from "@/lib/auth"

function errorToast(err: unknown, fallback: string) {
  toast.error(err instanceof ApiError ? err.message : fallback)
}

function Settings({ org, server }: { org: string; server: Server }) {
  const queryClient = useQueryClient()
  const router = useRouter()
  const [fields, setFields] = useState({
    name: server.name,
    mode: server.mode as string,
    track_opens: server.track_opens,
    track_clicks: server.track_clicks,
    bounce_hook_url: server.bounce_hook_url ?? "",
    delivery_hook_url: server.delivery_hook_url ?? "",
    inbound_domain: server.inbound_domain ?? "",
    spam_threshold: server.spam_threshold?.toString() ?? "",
  })
  const [deleteOpen, setDeleteOpen] = useState(false)
  const pools = useQuery({ queryKey: ["admin", "ip-pools"], queryFn: adminApi.ipPools.list })
  const { me } = useAuth()

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["server", org, server.permalink] })

  const save = useMutation({
    mutationFn: () =>
      adminApi.servers(org).update(server.permalink, {
        name: fields.name,
        mode: fields.mode as Server["mode"],
        track_opens: fields.track_opens,
        track_clicks: fields.track_clicks,
        ...(fields.bounce_hook_url ? { bounce_hook_url: fields.bounce_hook_url } : {}),
        ...(fields.delivery_hook_url ? { delivery_hook_url: fields.delivery_hook_url } : {}),
        ...(fields.inbound_domain ? { inbound_domain: fields.inbound_domain } : {}),
        ...(fields.spam_threshold
          ? { spam_threshold: Number(fields.spam_threshold) }
          : {}),
      }),
    onSuccess: () => {
      invalidate()
      toast.success("Server updated")
    },
    onError: (err) => errorToast(err, "Could not update the server"),
  })

  return (
    <div className="max-w-2xl space-y-6">
      <Card>
        <CardHeader>
          <CardTitle className="text-base">General</CardTitle>
        </CardHeader>
        <CardContent className="grid gap-4">
          <div className="grid grid-cols-2 gap-2">
            <div className="grid gap-2">
              <Label>Name</Label>
              <Input
                value={fields.name}
                onChange={(e) => setFields({ ...fields, name: e.target.value })}
              />
            </div>
            <div className="grid gap-2">
              <Label>Mode</Label>
              <Select
                value={fields.mode}
                onValueChange={(value) => setFields({ ...fields, mode: value })}
              >
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
          <div className="flex items-center gap-6">
            <div className="flex items-center gap-2">
              <Switch
                checked={fields.track_opens}
                onCheckedChange={(checked) => setFields({ ...fields, track_opens: checked })}
                id="track-opens"
              />
              <Label htmlFor="track-opens">Track opens</Label>
            </div>
            <div className="flex items-center gap-2">
              <Switch
                checked={fields.track_clicks}
                onCheckedChange={(checked) => setFields({ ...fields, track_clicks: checked })}
                id="track-clicks"
              />
              <Label htmlFor="track-clicks">Track clicks</Label>
            </div>
          </div>
          <div className="grid grid-cols-2 gap-2">
            <div className="grid gap-2">
              <Label>Bounce webhook URL</Label>
              <Input
                value={fields.bounce_hook_url}
                onChange={(e) => setFields({ ...fields, bounce_hook_url: e.target.value })}
                placeholder="https://…"
              />
            </div>
            <div className="grid gap-2">
              <Label>Delivery webhook URL</Label>
              <Input
                value={fields.delivery_hook_url}
                onChange={(e) => setFields({ ...fields, delivery_hook_url: e.target.value })}
                placeholder="https://…"
              />
            </div>
          </div>
          <div className="grid grid-cols-2 gap-2">
            <div className="grid gap-2">
              <Label>Inbound domain</Label>
              <Input
                value={fields.inbound_domain}
                onChange={(e) => setFields({ ...fields, inbound_domain: e.target.value })}
                placeholder="in.example.com"
              />
            </div>
            <div className="grid gap-2">
              <Label>Spam threshold</Label>
              <Input
                value={fields.spam_threshold}
                onChange={(e) => setFields({ ...fields, spam_threshold: e.target.value })}
                placeholder="5"
              />
            </div>
          </div>
          <Button className="justify-self-start" onClick={() => save.mutate()} disabled={save.isPending}>
            Save changes
          </Button>
        </CardContent>
      </Card>

      {me?.user.admin && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">IP pool</CardTitle>
            <CardDescription>Source addresses for this server's outbound mail.</CardDescription>
          </CardHeader>
          <CardContent>
            <Select
              value={server.ip_pool_id?.toString() ?? "none"}
              onValueChange={async (value) => {
                try {
                  await adminApi
                    .servers(org)
                    .setIpPool(server.permalink, value === "none" ? null : Number(value))
                  invalidate()
                } catch (err) {
                  errorToast(err, "Could not assign the IP pool")
                }
              }}
            >
              <SelectTrigger className="w-64">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value="none">Default pool</SelectItem>
                {pools.data?.ip_pools.map((pool) => (
                  <SelectItem key={pool.id} value={pool.id.toString()}>
                    {pool.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="text-base">Suspension & deletion</CardTitle>
        </CardHeader>
        <CardContent className="flex flex-wrap gap-2">
          {server.suspended ? (
            <Button
              variant="outline"
              onClick={async () => {
                try {
                  await adminApi.servers(org).unsuspend(server.permalink)
                  invalidate()
                } catch (err) {
                  errorToast(err, "Could not unsuspend")
                }
              }}
            >
              Unsuspend
            </Button>
          ) : (
            <Button
              variant="outline"
              onClick={async () => {
                try {
                  await adminApi.servers(org).suspend(server.permalink)
                  invalidate()
                } catch (err) {
                  errorToast(err, "Could not suspend")
                }
              }}
            >
              Suspend sending
            </Button>
          )}
          <Button variant="destructive" onClick={() => setDeleteOpen(true)}>
            Delete server
          </Button>
        </CardContent>
      </Card>

      <ConfirmDialog
        open={deleteOpen}
        onOpenChange={setDeleteOpen}
        title={`Delete ${server.name}?`}
        description="Removes the server with all domains, credentials and messages."
        onConfirm={async () => {
          try {
            await adminApi.servers(org).delete(server.permalink)
            router.push(`/orgs/${org}`)
          } catch (err) {
            errorToast(err, "Could not delete the server")
          }
        }}
      />
    </div>
  )
}

const TABS = [
  { value: "overview", label: "Settings" },
  { value: "domains", label: "Domains" },
  { value: "credentials", label: "Credentials" },
  { value: "routes", label: "Routes" },
  { value: "webhooks", label: "Webhooks" },
  { value: "suppressions", label: "Suppressions" },
  { value: "messaging", label: "Messaging" },
]

/// Header + tab bar of /orgs/[org]/servers/[server].
export function ServerShell({
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
  const serverQuery = useQuery({
    queryKey: ["server", org, server],
    queryFn: () => adminApi.servers(org).get(server),
  })
  const record = serverQuery.data?.server
  const segments = pathname.split(`/servers/${server}`)[1] ?? ""
  const tab = segments.split("/")[1] || "overview"

  return (
    <div>
      <PageHeader
        title={record ? record.name : server}
        description={`${org} / ${server}`}
        action={record?.suspended ? <Badge variant="destructive">suspended</Badge> : undefined}
      />
      <Tabs
        value={tab}
        onValueChange={(value) =>
          router.push(`/orgs/${org}/servers/${server}${value === "overview" ? "" : `/${value}`}`)
        }
      >
        <TabsList className="mb-4 flex-wrap">
          {TABS.map((t) => (
            <TabsTrigger key={t.value} value={t.value}>
              {t.label}
            </TabsTrigger>
          ))}
        </TabsList>
      </Tabs>
      {children}
    </div>
  )
}

/// The settings tab (index page of a server): loads the record, then
/// renders the form keyed by id so edits reset on server switch.
export function ServerSettingsPage({ org, server }: { org: string; server: string }) {
  const serverQuery = useQuery({
    queryKey: ["server", org, server],
    queryFn: () => adminApi.servers(org).get(server),
  })
  if (!serverQuery.data) {
    return <p className="text-sm text-muted-foreground">Loading…</p>
  }
  return <Settings org={org} server={serverQuery.data.server} key={serverQuery.data.server.id} />
}
