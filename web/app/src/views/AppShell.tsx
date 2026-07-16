"use client"

// The signed-in application frame, following the shadcn "dashboard-01"
// block: a collapsible icon sidebar (org switcher, servers of the
// active organization, org areas, instance admin), a header with the
// sidebar trigger, route breadcrumbs and the ⌘K hint, and the page
// content in the inset. Also hosts the global command palette.

import { Fragment, useEffect, useState, useSyncExternalStore } from "react"
import Link from "next/link"
import { useParams, usePathname, useRouter } from "next/navigation"
import { useQuery } from "@tanstack/react-query"
import {
  AtSignIcon,
  BadgeCheckIcon,
  BanIcon,
  BookOpenIcon,
  CheckIcon,
  ChevronDownIcon,
  ChevronsUpDownIcon,
  ClockIcon,
  CreditCardIcon,
  FileTextIcon,
  FingerprintIcon,
  GaugeIcon,
  GlobeIcon,
  InboxIcon,
  KeyRoundIcon,
  LayersIcon,
  LayoutDashboardIcon,
  LogOutIcon,
  NetworkIcon,
  NewspaperIcon,
  ScrollTextIcon,
  SearchIcon,
  SendIcon,
  ServerIcon,
  SettingsIcon,
  ShieldCheckIcon,
  UsersIcon,
  WebhookIcon,
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
import { Button, interactiveCard } from "@/components/ui/button"
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
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSkeleton,
  SidebarProvider,
} from "@/components/ui/sidebar"
import { CodePanel } from "@/components/code-panel"
import { CommandPalette } from "@/components/command-palette"
import { FormDialog, Kbd } from "@/components/form-dialog"
import { NEW_ORG_EVENT, OrgSwitcher } from "@/components/org-switcher"
import { adminApi, ApiError } from "@/lib/api"
import {
  getLastActiveOrg,
  serverDotColor,
  setLastActiveOrg,
  subscribeLastActiveOrg,
} from "@/lib/api-extras"
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
  sso: "Single sign-on",
  dmarc: "DMARC",
  recipients: "Recipients",
  messaging: "Messaging",
  messages: "Messages",
  queue: "Queue",
  stats: "Summary",
  statistics: "Statistics",
  streams: "Streams",
  templates: "Templates",
  users: "Users",
  "ip-pools": "IP pools",
  "api-keys": "Admin API keys",
  audit: "Audit log",
}

function segmentLabel(segment: string) {
  const decoded = decodeURIComponent(segment)
  if (SEGMENT_LABELS[decoded]) return SEGMENT_LABELS[decoded]
  // dynamic values (domain names, recipient addresses) render verbatim
  if (decoded.includes(".") || decoded.includes("@")) return decoded
  return decoded.charAt(0).toUpperCase() + decoded.slice(1).replaceAll("-", " ")
}

/// The areas of a single mail server, shown as a sub-menu beneath the
/// active server in the sidebar (this replaces the old in-page tab bar).
/// The server index is its Dashboard; everything else hangs off it.
function serverAreas(base: string) {
  return [
    { href: base, label: "Dashboard", icon: GaugeIcon, match: "exact" as const },
    { href: `${base}/messaging`, label: "Messaging", icon: SendIcon, match: "prefix" as const },
    { href: `${base}/streams`, label: "Streams", icon: LayersIcon, match: "prefix" as const },
    { href: `${base}/templates`, label: "Templates", icon: FileTextIcon, match: "prefix" as const },
    { href: `${base}/domains`, label: "Domains", icon: GlobeIcon, match: "prefix" as const },
    { href: `${base}/credentials`, label: "Credentials", icon: KeyRoundIcon, match: "prefix" as const },
    { href: `${base}/routes`, label: "Routes", icon: InboxIcon, match: "prefix" as const },
    { href: `${base}/webhooks`, label: "Webhooks", icon: WebhookIcon, match: "prefix" as const },
    { href: `${base}/sender-addresses`, label: "Senders", icon: AtSignIcon, match: "prefix" as const },
    { href: `${base}/suppressions`, label: "Suppressions", icon: BanIcon, match: "prefix" as const },
    { href: `${base}/dmarc`, label: "DMARC", icon: ShieldCheckIcon, match: "prefix" as const },
    { href: `${base}/queue`, label: "Queue", icon: ClockIcon, match: "prefix" as const },
    { href: `${base}/logs`, label: "API logs", icon: ScrollTextIcon, match: "prefix" as const },
    { href: `${base}/settings`, label: "Settings", icon: SettingsIcon, match: "prefix" as const },
  ]
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

/// The organization the sidebar works against: the routed one, else the
/// last one used (localStorage), else the first membership.
function useActiveOrg(): string | undefined {
  const { me } = useAuth()
  const { org } = useActiveParams()
  const stored = useSyncExternalStore(
    subscribeLastActiveOrg,
    getLastActiveOrg,
    () => null,
  )

  // Remember the routed org as "last active".
  useEffect(() => {
    if (org) setLastActiveOrg(org)
  }, [org])

  if (org) return org
  const memberships = me?.memberships ?? []
  if (stored && memberships.some((m) => m.organization.permalink === stored)) {
    return stored
  }
  return memberships[0]?.organization.permalink
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
    crumbs.push({ label: "Administration" })
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

// The signed-in user menu, rendered compactly in the top bar (right side):
// name + email to the left of the avatar, opening a dropdown downward.
function NavUser() {
  const { me, logout } = useAuth()
  const router = useRouter()

  const isAdmin = me?.user.admin ?? false
  const name = [me?.user.first_name, me?.user.last_name].filter(Boolean).join(" ")
  const initials =
    [me?.user.first_name?.[0], me?.user.last_name?.[0]].filter(Boolean).join("").toUpperCase() ||
    me?.user.email_address?.[0]?.toUpperCase() ||
    "?"

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className={`gap-2 ${interactiveCard}`}
        >
          <Avatar className="size-5 rounded-md">
            <AvatarFallback className="rounded-md text-[10px]">{initials}</AvatarFallback>
          </Avatar>
          <span className="hidden max-w-40 truncate font-medium sm:inline">{name}</span>
          <ChevronDownIcon className="size-4 text-muted-foreground" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-56 rounded-lg" side="bottom" align="end" sideOffset={8}>
        <DropdownMenuLabel>
          {name}
          {isAdmin && <span className="ml-1 text-xs text-muted-foreground">(admin)</span>}
        </DropdownMenuLabel>
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => router.push("/account")}>
          <BadgeCheckIcon /> Account & security
        </DropdownMenuItem>
        <DropdownMenuSeparator />
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
  )
}

function AppSidebar({ activeOrg }: { activeOrg: string | undefined }) {
  const { me } = useAuth()
  const router = useRouter()
  const pathname = usePathname() ?? ""
  const servers = useOrgServers(activeOrg)

  const isAdmin = me?.user.admin ?? false
  // Admin mode: the /admin/* area swaps the whole sidebar for the
  // instance-administration nav (entered via the "Admin" pill in the top
  // bar); the org/server nav is hidden until you leave it.
  const isAdminMode = pathname.startsWith("/admin")

  // The server whose nav fills the sidebar: the one in the URL, else the
  // first available. The header switcher changes it; its areas render as
  // the sidebar's primary nav.
  const serverList = servers.data?.servers ?? []
  const activeServerPermalink = pathname.match(/\/orgs\/[^/]+\/servers\/([^/]+)/)?.[1]
  const activeServer =
    serverList.find((s) => s.permalink === activeServerPermalink) ?? serverList[0]
  const serverBase =
    activeOrg && activeServer ? `/orgs/${activeOrg}/servers/${activeServer.permalink}` : null
  const serverNav = serverBase ? serverAreas(serverBase) : []
  const role = me?.memberships.find(
    (m) => m.organization.permalink === activeOrg,
  )?.role
  const canBilling = isAdmin || role === "owner" || role === "admin"

  // `enabled: false` (the self-hosted default) hides the Billing entry.
  const billing = useQuery({
    queryKey: ["billing", activeOrg],
    queryFn: () => adminApi.billing(activeOrg!).get(),
    enabled: !!activeOrg && canBilling,
    retry: false,
  })
  const showBilling = billing.data?.enabled === true

  const orgBase = activeOrg ? `/orgs/${activeOrg}` : null
  type OrgArea = {
    href: string
    label: string
    icon: typeof UsersIcon
    match: "exact" | "prefix" | "never"
  }
  const orgAreas: OrgArea[] = orgBase
    ? [
        { href: orgBase, label: "Dashboard", icon: LayoutDashboardIcon, match: "exact" },
        { href: `${orgBase}/servers`, label: "Servers", icon: ServerIcon, match: "exact" },
        { href: `${orgBase}/members`, label: "Members", icon: UsersIcon, match: "prefix" },
        // tenant SSO configuration carries provider secrets — admin+ only
        ...(canBilling
          ? [
              {
                href: `${orgBase}/sso`,
                label: "Single sign-on",
                icon: FingerprintIcon,
                match: "prefix" as const,
              },
            ]
          : []),
        { href: `${orgBase}/settings`, label: "Settings", icon: SettingsIcon, match: "prefix" },
        ...(showBilling
          ? [
              {
                href: `${orgBase}/billing`,
                label: "Billing",
                icon: CreditCardIcon,
                match: "prefix" as const,
              },
            ]
          : []),
      ]
    : []

  return (
    <Sidebar
      collapsible="icon"
      variant="inset"
      className="md:[&_[data-slot=sidebar-inner]]:bg-transparent"
    >
      <SidebarHeader>
        {/* The official wordmark — a plain link to /dashboard, no button
            chrome. Dark-slate PNG flipped to white in dark mode. */}
        <Link href="/dashboard" className="inline-block px-2 pt-0 pb-[3px]">
          {/* eslint-disable-next-line @next/next/no-img-element */}
          <img
            src="/camelmailer-logo.png"
            alt="CamelMailer"
            className="block h-auto w-44 dark:brightness-0 dark:invert"
          />
        </Link>
        {!isAdminMode && activeOrg && (
          <>
            <div aria-hidden className="mt-1.5 mb-2 border-b border-black/5 dark:border-white/10" />
            <SidebarMenu>
              <SidebarMenuItem>
                <DropdownMenu>
                  <DropdownMenuTrigger asChild>
                    <SidebarMenuButton
                      size="lg"
                      tooltip="Switch server"
                    className="rounded-lg border border-border bg-card shadow-sm data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
                  >
                    <div className="grid flex-1 text-left leading-tight">
                      <span className="truncate text-xs text-muted-foreground">Server</span>
                      <span className="flex items-center gap-2 truncate text-sm font-medium">
                        {activeServer && (
                          <span
                            aria-hidden
                            className="size-2 shrink-0 rounded-full"
                            style={{ backgroundColor: serverDotColor(activeServer) }}
                          />
                        )}
                        <span className="truncate">
                          {activeServer?.name ?? "Select server"}
                        </span>
                      </span>
                    </div>
                    <ChevronsUpDownIcon className="ml-auto size-4" />
                  </SidebarMenuButton>
                </DropdownMenuTrigger>
                <DropdownMenuContent
                  className="w-(--radix-dropdown-menu-trigger-width) min-w-56 rounded-lg"
                  side="right"
                  align="start"
                  sideOffset={4}
                >
                  <DropdownMenuLabel className="text-xs text-muted-foreground">
                    Servers
                  </DropdownMenuLabel>
                  {serverList.map((s) => (
                    <DropdownMenuItem
                      key={s.id}
                      onClick={() => router.push(`/orgs/${activeOrg}/servers/${s.permalink}`)}
                    >
                      <span
                        aria-hidden
                        className="size-2 shrink-0 rounded-full"
                        style={{ backgroundColor: serverDotColor(s) }}
                      />
                      <span className="truncate">{s.name}</span>
                      {activeServer?.permalink === s.permalink && (
                        <CheckIcon className="ml-auto size-4" />
                      )}
                    </DropdownMenuItem>
                  ))}
                    {serverList.length === 0 && (
                      <p className="px-2 py-1.5 text-xs text-muted-foreground">No servers yet.</p>
                    )}
                  </DropdownMenuContent>
                </DropdownMenu>
              </SidebarMenuItem>
            </SidebarMenu>
          </>
        )}
      </SidebarHeader>
      <SidebarContent className="overflow-x-hidden">
        {!isAdminMode && activeOrg && (
          <SidebarGroup className="pt-0">
            <SidebarGroupContent>
              <SidebarMenu>
                {servers.isPending &&
                  Array.from({ length: 5 }).map((_, i) => (
                    <SidebarMenuItem key={i}>
                      <SidebarMenuSkeleton showIcon />
                    </SidebarMenuItem>
                  ))}
                {serverNav.map(({ href, label, icon: Icon, match }) => (
                  <SidebarMenuItem key={label}>
                    <SidebarMenuButton
                      asChild
                      isActive={
                        match === "exact"
                          ? pathname === href
                          : pathname === href || pathname.startsWith(`${href}/`)
                      }
                      tooltip={label}
                    >
                      <Link href={href}>
                        <Icon />
                        <span>{label}</span>
                      </Link>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                ))}
                {servers.isSuccess && serverList.length === 0 && (
                  <p className="px-2 py-1 text-xs text-muted-foreground group-data-[collapsible=icon]:hidden">
                    No servers yet.
                  </p>
                )}
              </SidebarMenu>
            </SidebarGroupContent>
          </SidebarGroup>
        )}
        {!isAdminMode && activeOrg && (
          <div aria-hidden className="mx-3 my-0 border-b border-black/5 dark:border-white/10" />
        )}
        {!isAdminMode && activeOrg && (
          <SidebarGroup>
            <SidebarGroupLabel className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
              Organization
            </SidebarGroupLabel>
            <SidebarGroupContent>
              <SidebarMenu>
                {orgAreas.map(({ href, label, icon: Icon, match }) => (
                  <SidebarMenuItem key={label}>
                    <SidebarMenuButton
                      asChild
                      isActive={
                        match === "exact"
                          ? pathname === href
                          : match === "prefix"
                            ? pathname === href || pathname.startsWith(`${href}/`)
                            : false
                      }
                      tooltip={label}
                    >
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
        {isAdminMode && (
          <SidebarGroup>
            <SidebarGroupLabel className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">
              Administration
            </SidebarGroupLabel>
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
        <SidebarMenu>
          {isAdmin && (
            <SidebarMenuItem>
              <SidebarMenuButton asChild isActive={isAdminMode} tooltip="Admin">
                <Link href="/admin/users">
                  <ShieldCheckIcon />
                  <span>Admin</span>
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          )}
          <SidebarMenuItem>
            <SidebarMenuButton asChild tooltip="Docs">
              <a href="https://camelmailer.com/docs" target="_blank" rel="noreferrer">
                <BookOpenIcon />
                <span>Docs</span>
              </a>
            </SidebarMenuButton>
          </SidebarMenuItem>
          <SidebarMenuItem>
            <SidebarMenuButton asChild tooltip="Changelog">
              <a
                href="https://github.com/camelmailer/camelmailer/releases"
                target="_blank"
                rel="noreferrer"
              >
                <NewspaperIcon />
                <span>Changelog</span>
              </a>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>
    </Sidebar>
  )
}

export default function AppShell({ children }: { children: React.ReactNode }) {
  const { refresh } = useAuth()
  const router = useRouter()
  const { org } = useActiveParams()
  const activeOrg = useActiveOrg()
  const [newOrgOpen, setNewOrgOpen] = useState(false)
  const [newOrgName, setNewOrgName] = useState("")
  const [busy, setBusy] = useState(false)
  const [paletteOpen, setPaletteOpen] = useState(false)

  // Empty states and the command palette open the dialog via this event
  // (see components/org-switcher.tsx).
  useEffect(() => {
    const onNewOrg = () => setNewOrgOpen(true)
    window.addEventListener(NEW_ORG_EVENT, onNewOrg)
    return () => window.removeEventListener(NEW_ORG_EVENT, onNewOrg)
  }, [])

  async function createOrg() {
    setBusy(true)
    try {
      const { organization } = await adminApi.organizations.create(newOrgName)
      // Give every new org a Production (Live) and a Development server up
      // front, so the dashboard is usable without a manual setup step. Best
      // effort: a failure here must not lose the freshly-created org.
      try {
        await adminApi.servers(organization.permalink).create("Production", "Live")
        await adminApi.servers(organization.permalink).create("Development", "Development")
      } catch (serverErr) {
        toast.error(
          serverErr instanceof ApiError
            ? serverErr.message
            : "Organization created, but its servers could not be set up",
        )
      }
      await refresh()
      setNewOrgOpen(false)
      setNewOrgName("")
      setLastActiveOrg(organization.permalink)
      router.push(`/orgs/${organization.permalink}`)
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Could not create the organization")
    } finally {
      setBusy(false)
    }
  }

  return (
    <SidebarProvider className="h-svh overflow-hidden has-data-[variant=inset]:bg-background">
      <AppSidebar activeOrg={activeOrg} />
      <SidebarInset className="flex min-h-0 min-w-0 flex-col gap-3 p-3 md:peer-data-[variant=inset]:m-0 md:peer-data-[variant=inset]:rounded-none md:peer-data-[variant=inset]:shadow-none">
        {/* Top bar over the halo: search far left, signed-in user far right.
            No left padding so the search edge lines up with the halo frame's
            left edge below it; pr-3 puts the account block at the same right
            gutter as the API button (24px from the window edge). */}
        <div className="flex shrink-0 items-center gap-2 pr-3">
          <Button
            variant="ghost"
            size="sm"
            className={`w-80 max-w-full justify-start gap-2 text-muted-foreground ${interactiveCard}`}
            onClick={() => setPaletteOpen(true)}
          >
            <SearchIcon className="size-4" />
            <span>Search…</span>
            <Kbd className="ml-auto">⌘K</Kbd>
          </Button>
          <div className="ml-auto flex items-center gap-2">
            <OrgSwitcher activeOrg={activeOrg} />
            <NavUser />
          </div>
        </div>
        {/* The workspace lives inside a halo frame — the same treatment as
            the auth cards and dialogs. It fills the remaining height; only
            the content body inside scrolls, never the page. */}
        <div className="-mr-3 flex min-h-0 min-w-0 flex-1 flex-col rounded-l-[1.875rem] shadow-[inset_0_0_2px_1px_#ffffff4d] ring-1 ring-black/5 dark:shadow-[inset_0_0_2px_1px_rgba(255,255,255,0.12)] dark:ring-white/10">
          <div className="flex min-h-0 min-w-0 flex-1 flex-col rounded-l-[1.875rem] py-1.5 pl-1.5 shadow-md shadow-black/5">
            <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden rounded-l-3xl bg-card text-card-foreground ring-1 ring-black/5 shadow-[0_8px_24px_-12px_rgba(86,47,0,0.22)] dark:shadow-[0_8px_24px_-12px_rgba(0,0,0,0.5)] dark:ring-white/10">
              {/* Fixed breadcrumb row — min-height matches a sm button + py so
                  it never jumps when the API button (CodePanel) is absent. */}
              <div className="flex min-h-14 shrink-0 items-center gap-2 border-b px-6 py-3">
                <AppBreadcrumbs />
                <div className="ml-auto">
                  <CodePanel />
                </div>
              </div>
              {/* Content body. Plain pages scroll here (p-6); pages that use
                  the <Page> scaffold fill this box (h-full) and manage their
                  own sticky header + internal scroll, so only their body
                  scrolls. overflow-y-auto keeps a scrollbar reserved for the
                  scrolling case without forcing one on filled pages. */}
              <div
                className="min-h-0 min-w-0 flex-1 overflow-y-auto p-6"
                key={org ?? "-"}
              >
                {children}
              </div>
            </div>
          </div>
        </div>
      </SidebarInset>

      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        activeOrg={activeOrg}
        onCreateOrganization={() => setNewOrgOpen(true)}
      />

      <FormDialog
        open={newOrgOpen}
        onOpenChange={setNewOrgOpen}
        title="New organization"
        submitLabel="Create"
        onSubmit={createOrg}
        busy={busy}
        submitDisabled={!newOrgName.trim()}
      >
        <div className="grid gap-2">
          <Label htmlFor="org-name">Name</Label>
          <Input
            id="org-name"
            value={newOrgName}
            onChange={(e) => setNewOrgName(e.target.value)}
            placeholder="Acme Inc"
          />
        </div>
      </FormDialog>
    </SidebarProvider>
  )
}
