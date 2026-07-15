"use client"

// Mail-server home: settings + resource tabs + messaging, routed under
// /orgs/:org/servers/:server.

import { useState } from "react"
import Link from "next/link"
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { useRouter } from "next/navigation"
import {
  ArrowRightIcon,
  CircleCheckIcon,
  CircleIcon,
  GlobeIcon,
  KeyRoundIcon,
  SendIcon,
  SettingsIcon,
  WebhookIcon,
} from "lucide-react"
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
import { adminApi, ApiError, type Server } from "@/lib/api"
import { useAuth } from "@/lib/auth"
import { ServerSummary } from "@/views/server/Messaging"
import { SetupTab } from "@/views/server/SetupTab"
import { Statistics } from "@/views/server/Statistics"

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

/// Server-scoped pages share the app sidebar's server sub-menu for
/// navigation, so the shell is now a thin passthrough (the in-page tab
/// bar moved into the sidebar).
export function ServerShell({ children }: { children: React.ReactNode }) {
  return <>{children}</>
}

/// The Settings page (/orgs/[org]/servers/[server]/settings): loads the
/// record, then renders the form keyed by id so edits reset on server
/// switch.
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

/// One "next step" row of the getting-started card: a done/undone marker,
/// a label, and a link to the area that completes it.
function SetupStep({
  done,
  label,
  href,
  cta,
}: {
  done: boolean
  label: string
  href: string
  cta: string
}) {
  return (
    <div className="flex items-center gap-3 py-1">
      {done ? (
        <CircleCheckIcon className="size-5 shrink-0 text-primary" />
      ) : (
        <CircleIcon className="size-5 shrink-0 text-muted-foreground/50" />
      )}
      <span className={done ? "text-muted-foreground line-through" : "font-medium"}>{label}</span>
      {!done && (
        <Button variant="outline" size="sm" className="ml-auto" asChild>
          <Link href={href}>{cta}</Link>
        </Button>
      )}
    </div>
  )
}

/// A quick-access tile linking to one area of the server.
function AreaCard({
  href,
  icon: Icon,
  title,
  description,
}: {
  href: string
  icon: typeof GlobeIcon
  title: string
  description: string
}) {
  return (
    <Link href={href} className="group">
      <Card className="h-full transition-colors hover:border-primary/40 hover:bg-accent/40">
        <CardContent className="flex items-start gap-3 p-4">
          <span className="flex size-9 shrink-0 items-center justify-center rounded-md bg-primary/10 text-primary">
            <Icon className="size-4" />
          </span>
          <div className="min-w-0">
            <p className="flex items-center gap-1 font-medium">
              {title}
              <ArrowRightIcon className="size-3.5 opacity-0 transition-opacity group-hover:opacity-60" />
            </p>
            <p className="text-sm text-muted-foreground">{description}</p>
          </div>
        </CardContent>
      </Card>
    </Link>
  )
}

/// The server landing page (/orgs/[org]/servers/[server]): a short status
/// summary, a getting-started card while setup is incomplete, and quick
/// access to every area of the server. This is where you enter a server
/// now — Settings moved to its own page.
export function ServerDashboard({ org, server }: { org: string; server: string }) {
  const serverQuery = useQuery({
    queryKey: ["server", org, server],
    queryFn: () => adminApi.servers(org).get(server),
  })
  const domains = useQuery({
    queryKey: ["domains", org, server],
    queryFn: () => adminApi.domains(org, server).list(),
  })
  const credentials = useQuery({
    queryKey: ["credentials", org, server],
    queryFn: () => adminApi.credentials(org, server).list(),
  })

  const record = serverQuery.data?.server
  const base = `/orgs/${org}/servers/${server}`
  const domainList = domains.data?.domains ?? []
  const verifiedDomains = domainList.filter((d) => d.verified).length
  const credentialCount = credentials.data?.credentials.length ?? 0
  const hasDomain = domainList.length > 0
  const hasVerifiedDomain = verifiedDomains > 0
  const hasCredential = credentialCount > 0
  const setupComplete = hasVerifiedDomain && hasCredential

  if (!record) {
    return <p className="text-sm text-muted-foreground">Loading…</p>
  }

  return (
    <div className="space-y-6">
      <PageHeader
        title={record.name}
        description={`${org} / ${server}`}
        action={
          <div className="flex items-center gap-2">
            <Badge variant={record.mode === "Live" ? "default" : "secondary"}>{record.mode}</Badge>
            {record.suspended && <Badge variant="destructive">suspended</Badge>}
          </div>
        }
      />

      <div className="grid grid-cols-2 gap-4 *:data-[slot=card]:bg-gradient-to-t *:data-[slot=card]:from-primary/5 *:data-[slot=card]:to-card *:data-[slot=card]:shadow-xs lg:grid-cols-4">
        <Card>
          <CardHeader className="pb-2">
            <CardDescription>Mode</CardDescription>
            <CardTitle className="text-2xl">{record.mode}</CardTitle>
          </CardHeader>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardDescription>Domains</CardDescription>
            <CardTitle className="text-2xl">
              {verifiedDomains}
              <span className="text-base font-normal text-muted-foreground"> / {domainList.length}</span>
            </CardTitle>
          </CardHeader>
          <CardContent className="pt-0 text-xs text-muted-foreground">verified</CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardDescription>Credentials</CardDescription>
            <CardTitle className="text-2xl">{credentialCount}</CardTitle>
          </CardHeader>
          <CardContent className="pt-0 text-xs text-muted-foreground">API &amp; SMTP keys</CardContent>
        </Card>
        <Card>
          <CardHeader className="pb-2">
            <CardDescription>Status</CardDescription>
            <CardTitle className="text-2xl">{record.suspended ? "Suspended" : "Active"}</CardTitle>
          </CardHeader>
          <CardContent className="pt-0 text-xs text-muted-foreground">
            {record.suspended ? "sending paused" : "ready to send"}
          </CardContent>
        </Card>
      </div>

      {!setupComplete && (
        <Card>
          <CardHeader>
            <CardTitle className="text-base">Get this server sending</CardTitle>
            <CardDescription>
              Two steps and your application can send transactional mail through this server.
            </CardDescription>
          </CardHeader>
          <CardContent className="divide-y">
            <SetupStep
              done={hasVerifiedDomain}
              label={
                hasDomain && !hasVerifiedDomain
                  ? "Verify your sending domain (SPF + DKIM)"
                  : "Add and verify a sending domain"
              }
              href={`${base}/domains`}
              cta={hasDomain ? "Verify" : "Add domain"}
            />
            <SetupStep
              done={hasCredential}
              label="Create an API key or SMTP credential"
              href={`${base}/credentials`}
              cta="Create credential"
            />
          </CardContent>
        </Card>
      )}

      <div>
        <h2 className="mb-3 text-sm font-medium text-muted-foreground">Setup</h2>
        <SetupTab org={org} server={server} />
      </div>

      <Statistics org={org} server={server} />

      <div>
        <h2 className="mb-3 text-sm font-medium text-muted-foreground">Manage this server</h2>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
          <AreaCard
            href={`${base}/messaging`}
            icon={SendIcon}
            title="Messaging"
            description="Messages, queue, streams and templates."
          />
          <AreaCard
            href={`${base}/domains`}
            icon={GlobeIcon}
            title="Domains"
            description="Sending domains and their DNS health."
          />
          <AreaCard
            href={`${base}/credentials`}
            icon={KeyRoundIcon}
            title="Credentials"
            description="API keys and SMTP connection details."
          />
          <AreaCard
            href={`${base}/webhooks`}
            icon={WebhookIcon}
            title="Webhooks"
            description="HTTP callbacks for message events."
          />
          <AreaCard
            href={`${base}/settings`}
            icon={SettingsIcon}
            title="Settings"
            description="Mode, tracking, IP pool and deletion."
          />
        </div>
      </div>

      <div>
        <h2 className="mb-3 text-sm font-medium text-muted-foreground">Summary</h2>
        <ServerSummary org={org} server={server} />
      </div>
    </div>
  )
}
