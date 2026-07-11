"use client"

// The signed-in application frame, following the shadcn "dashboard-01"
// block: a collapsible icon sidebar (organizations, servers of the active
// organization, instance admin), a header with the sidebar trigger and
// route breadcrumbs, and the page content in the inset.

import { Fragment, useState } from "react"
import Link from "next/link"
import { useParams, usePathname, useRouter } from "next/navigation"
import { useQuery } from "@tanstack/react-query"
import {
  BadgeCheckIcon,
  BuildingIcon,
  ChevronsUpDownIcon,
  KeyRoundIcon,
  ListIcon,
  LogOutIcon,
  NetworkIcon,
  PlusIcon,
  ScrollTextIcon,
  ServerIcon,
  UsersIcon,
} from "lucide-react"
import { Avatar, AvatarFallback } from "@/components/ui/avatar"
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb"
import { Button } from "@/components/ui/button"
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
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Separator } from "@/components/ui/separator"
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupAction,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSkeleton,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from "@/components/ui/sidebar"
import { adminApi, ApiError } from "@/lib/api"
import { useAuth } from "@/lib/auth"
import { toast } from "sonner"

/// Labels for the route segments that appear in breadcrumbs.
const SEGMENT_LABELS: Record<string, string> = {
  members: "Members",
  invitations: "Invitations",
  settings: "Settings",
  billing: "Billing",
  domains: "Domains",
  credentials: "Credentials",
  routes: "Routes",
  webhooks: "Webhooks",
  suppressions: "Suppressions",
  "sender-addresses": "Sender addresses",
  dmarc: "DMARC",
  messaging: "Messaging",
  messages: "Messages",
  queue: "Queue",
  stats: "Statistics",
  streams: "Streams",
  templates: "Templates",
  users: "Users",
  "ip-pools": "IP pools",
  "api-keys": "Admin API keys",
  audit: "Audit log",
}

function segmentLabel(segment: string) {
  return (
    SEGMENT_LABELS[segment] ??
    segment.charAt(0).toUpperCase() + segment.slice(1).replaceAll("-", " ")
  )
}

/// The servers of the active organization — shared (via the query key)
/// with the org pages, so the sidebar and breadcrumbs stay warm.
function useOrgServers(org: string | undefined) {
  return useQuery({
    queryKey: ["servers", org],
    queryFn: () => adminApi.servers(org!).list(),
    enabled: !!org,
  })
}

function useActiveParams() {
  const params = useParams()
  return {
    org: typeof params?.org === "string" ? params.org : undefined,
    server: typeof params?.server === "string" ? params.server : undefined,
  }
}

function AppBreadcrumbs() {
  const pathname = usePathname() ?? ""
  const { me } = useAuth()
  const { org, server } = useActiveParams()
  const servers = useOrgServers(org)

  const segments = pathname.split("/").filter(Boolean)
  const crumbs: { label: string; href?: string }[] = [
    { label: "Dashboard", href: pathname === "/dashboard" ? undefined : "/dashboard" },
  ]

  if (segments[0] === "orgs" && !org) {
    crumbs.push({ label: "All organizations" })
  } else if (org) {
    const orgName =
      me?.memberships.find((m) => m.organization.permalink === org)?.organization.name ?? org
    const orgHref = `/orgs/${org}`
    crumbs.push({ label: orgName, href: segments.length > 2 ? orgHref : undefined })
    if (server) {
      const serverName =
        servers.data?.servers.find((s) => s.permalink === server)?.name ?? server
      const serverHref = `${orgHref}/servers/${server}`
      const rest = segments.slice(4)
      crumbs.push({ label: serverName, href: rest.length > 0 ? serverHref : undefined })
      rest.forEach((segment, i) => {
        crumbs.push({
          label: segmentLabel(segment),
          href:
            i < rest.length - 1
              ? `${serverHref}/${rest.slice(0, i + 1).join("/")}`
              : undefined,
        })
      })
    } else if (segments[2]) {
      crumbs.push({ label: segmentLabel(segments[2]) })
    }
  } else if (segments[0] === "account") {
    crumbs.push({ label: "Account & security" })
  } else if (segments[0] === "admin") {
    crumbs.push({ label: "Instance admin" })
    if (segments[1]) crumbs.push({ label: segmentLabel(segments[1]) })
  }

  return (
    <Breadcrumb>
      <BreadcrumbList>
        {crumbs.map((crumb, i) => {
          const isLast = i === crumbs.length - 1
          return (
            <Fragment key={`${crumb.label}-${i}`}>
              {i > 0 && <BreadcrumbSeparator className="hidden md:block" />}
              <BreadcrumbItem className={isLast ? undefined : "hidden md:block"}>
                {crumb.href ? (
                  <BreadcrumbLink asChild>
                    <Link href={crumb.href}>{crumb.label}</Link>
                  </BreadcrumbLink>
                ) : isLast ? (
                  <BreadcrumbPage>{crumb.label}</BreadcrumbPage>
                ) : (
                  <span>{crumb.label}</span>
                )}
              </BreadcrumbItem>
            </Fragment>
          )
        })}
      </BreadcrumbList>
    </Breadcrumb>
  )
}

function NavUser() {
  const { me, logout } = useAuth()
  const router = useRouter()
  const { isMobile } = useSidebar()

  const isAdmin = me?.user.admin ?? false
  const name = [me?.user.first_name, me?.user.last_name].filter(Boolean).join(" ")
  const initials =
    [me?.user.first_name?.[0], me?.user.last_name?.[0]].filter(Boolean).join("").toUpperCase() ||
    me?.user.email_address?.[0]?.toUpperCase() ||
    "?"

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <SidebarMenuButton
              size="lg"
              className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
            >
              <Avatar className="size-8 rounded-lg">
                <AvatarFallback className="rounded-lg">{initials}</AvatarFallback>
              </Avatar>
              <div className="grid flex-1 text-left text-sm leading-tight">
                <span className="truncate font-medium">{name}</span>
                <span className="truncate text-xs text-muted-foreground">
                  {me?.user.email_address}
                </span>
              </div>
              <ChevronsUpDownIcon className="ml-auto size-4" />
            </SidebarMenuButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            className="w-(--radix-dropdown-menu-trigger-width) min-w-56 rounded-lg"
            side={isMobile ? "bottom" : "right"}
            align="end"
            sideOffset={4}
          >
            <DropdownMenuLabel>
              {name}
              {isAdmin && <span className="ml-1 text-xs text-muted-foreground">(admin)</span>}
            </DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => router.push("/account")}>
              <BadgeCheckIcon /> Account & security
            </DropdownMenuItem>
            <DropdownMenuItem
              onClick={async () => {
                await logout()
                router.push("/login")
              }}
            >
              <LogOutIcon /> Sign out
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    </SidebarMenu>
  )
}

function AppSidebar({ onNewOrganization }: { onNewOrganization: () => void }) {
  const { me } = useAuth()
  const pathname = usePathname() ?? ""
  const { org } = useActiveParams()
  const servers = useOrgServers(org)

  const memberships = me?.memberships ?? []
  const isAdmin = me?.user.admin ?? false
  const activeOrgName =
    memberships.find((m) => m.organization.permalink === org)?.organization.name ?? org

  return (
    <Sidebar collapsible="icon">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" asChild tooltip="Dashboard">
              <Link href="/dashboard">
                <div className="flex aspect-square size-8 items-center justify-center">
                  {/* eslint-disable-next-line @next/next/no-img-element */}
                  <img src="/camelmailer-symbol.png" alt="" className="size-6" />
                </div>
                <span className="truncate text-base font-semibold">CamelMailer</span>
              </Link>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        <SidebarGroup>
          <SidebarGroupLabel>Organizations</SidebarGroupLabel>
          <SidebarGroupAction title="New organization" onClick={onNewOrganization}>
            <PlusIcon /> <span className="sr-only">New organization</span>
          </SidebarGroupAction>
          <SidebarGroupContent>
            <SidebarMenu>
              {memberships.map(({ organization, role }) => {
                const href = `/orgs/${organization.permalink}`
                const isActive =
                  pathname === href ||
                  (pathname.startsWith(`${href}/`) && !pathname.startsWith(`${href}/servers/`))
                return (
                  <SidebarMenuItem key={organization.id}>
                    <SidebarMenuButton asChild isActive={isActive} tooltip={organization.name}>
                      <Link href={href}>
                        <BuildingIcon />
                        <span className="truncate">{organization.name}</span>
                      </Link>
                    </SidebarMenuButton>
                    <SidebarMenuBadge className="text-[10px] font-normal text-muted-foreground">
                      {role}
                    </SidebarMenuBadge>
                  </SidebarMenuItem>
                )
              })}
              {isAdmin && (
                <SidebarMenuItem>
                  <SidebarMenuButton
                    asChild
                    isActive={pathname === "/orgs"}
                    tooltip="All organizations"
                  >
                    <Link href="/orgs">
                      <ListIcon />
                      <span className="text-muted-foreground">All organizations…</span>
                    </Link>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              )}
              {memberships.length === 0 && !isAdmin && (
                <p className="px-2 py-1 text-xs text-muted-foreground group-data-[collapsible=icon]:hidden">
                  No memberships yet.
                </p>
              )}
            </SidebarMenu>
          </SidebarGroupContent>
        </SidebarGroup>
        {org && (
          <SidebarGroup>
            <SidebarGroupLabel className="truncate">
              {activeOrgName} — servers
            </SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {servers.isPending &&
                  Array.from({ length: 2 }).map((_, i) => (
                    <SidebarMenuItem key={i}>
                      <SidebarMenuSkeleton showIcon />
                    </SidebarMenuItem>
                  ))}
                {(servers.data?.servers ?? []).map((server) => {
                  const href = `/orgs/${org}/servers/${server.permalink}`
                  return (
                    <SidebarMenuItem key={server.id}>
                      <SidebarMenuButton
                        asChild
                        isActive={pathname === href || pathname.startsWith(`${href}/`)}
                        tooltip={server.name}
                      >
                        <Link href={href}>
                          <ServerIcon />
                          <span className="truncate">{server.name}</span>
                        </Link>
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  )
                })}
                {servers.isSuccess && servers.data.servers.length === 0 && (
                  <p className="px-2 py-1 text-xs text-muted-foreground group-data-[collapsible=icon]:hidden">
                    No servers yet.
                  </p>
                )}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        )}
        {isAdmin && (
          <SidebarGroup>
            <SidebarGroupLabel>Instance admin</SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {(
                  [
                    ["/admin/users", "Users", UsersIcon],
                    ["/admin/ip-pools", "IP pools", NetworkIcon],
                    ["/admin/api-keys", "Admin API keys", KeyRoundIcon],
                    ["/admin/audit", "Audit log", ScrollTextIcon],
                  ] as const
                ).map(([href, label, Icon]) => (
                  <SidebarMenuItem key={href}>
                    <SidebarMenuButton asChild isActive={pathname === href} tooltip={label}>
                      <Link href={href}>
                        <Icon />
                        <span>{label}</span>
                      </Link>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                ))}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        )}
      </SidebarContent>
      <SidebarFooter>
        <NavUser />
      </SidebarFooter>
    </Sidebar>
  )
}

export default function AppShell({ children }: { children: React.ReactNode }) {
  const { refresh } = useAuth()
  const router = useRouter()
  const { org } = useActiveParams()
  const [newOrgOpen, setNewOrgOpen] = useState(false)
  const [newOrgName, setNewOrgName] = useState("")
  const [busy, setBusy] = useState(false)

  async function createOrg() {
    setBusy(true)
    try {
      const { organization } = await adminApi.organizations.create(newOrgName)
      await refresh()
      setNewOrgOpen(false)
      setNewOrgName("")
      router.push(`/orgs/${organization.permalink}`)
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Could not create the organization")
    } finally {
      setBusy(false)
    }
  }

  return (
    <SidebarProvider>
      <AppSidebar onNewOrganization={() => setNewOrgOpen(true)} />
      <SidebarInset>
        <header className="flex h-14 shrink-0 items-center gap-2 border-b px-4">
          <SidebarTrigger className="-ml-1" />
          <Separator
            orientation="vertical"
            className="mr-2 data-[orientation=vertical]:h-4"
          />
          <AppBreadcrumbs />
        </header>
        <main className="min-w-0 flex-1 p-6" key={org ?? "-"}>
          {children}
        </main>
      </SidebarInset>

      <Dialog open={newOrgOpen} onOpenChange={setNewOrgOpen}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New organization</DialogTitle>
          </DialogHeader>
          <div className="grid gap-2">
            <Label htmlFor="org-name">Name</Label>
            <Input
              id="org-name"
              value={newOrgName}
              onChange={(e) => setNewOrgName(e.target.value)}
              placeholder="Acme Inc"
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setNewOrgOpen(false)}>
              Cancel
            </Button>
            <Button onClick={createOrg} disabled={busy || !newOrgName.trim()}>
              Create
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </SidebarProvider>
  )
}
