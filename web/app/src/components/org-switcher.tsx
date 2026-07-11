"use client"

// The workspace-style organization switcher in the sidebar header:
// current org as initials avatar + name, dropdown with all memberships
// (checkmark on the active one), then "Create organization". Picking an
// org navigates to its overview and remembers it in localStorage.

import { useRouter } from "next/navigation"
import { CheckIcon, ChevronsUpDownIcon, ListIcon, PlusIcon } from "lucide-react"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar"
import { setLastActiveOrg } from "@/lib/api-extras"
import { useAuth } from "@/lib/auth"

/// Ask the app shell to open the "New organization" dialog from
/// anywhere (empty states, command palette) without prop drilling.
export const NEW_ORG_EVENT = "camelmailer:new-organization"

export function requestNewOrganization() {
  if (typeof window !== "undefined") window.dispatchEvent(new Event(NEW_ORG_EVENT))
}

export function orgInitials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean)
  return (
    words
      .slice(0, 2)
      .map((word) => word[0]!.toUpperCase())
      .join("") || "?"
  )
}

export function OrgSwitcher({ activeOrg }: { activeOrg: string | undefined }) {
  const { me } = useAuth()
  const router = useRouter()
  const { isMobile } = useSidebar()

  const memberships = me?.memberships ?? []
  const isAdmin = me?.user.admin ?? false
  const active = memberships.find((m) => m.organization.permalink === activeOrg)

  function switchTo(permalink: string) {
    setLastActiveOrg(permalink)
    router.push(`/orgs/${permalink}`)
  }

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <SidebarMenuButton
              size="lg"
              tooltip="Switch organization"
              className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
            >
              <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-sidebar-primary text-xs font-semibold text-sidebar-primary-foreground">
                {active ? orgInitials(active.organization.name) : "—"}
              </div>
              <div className="grid flex-1 text-left text-sm leading-tight">
                <span className="truncate font-medium">
                  {active?.organization.name ?? activeOrg ?? "Select organization"}
                </span>
                <span className="truncate text-xs text-muted-foreground">
                  {active ? active.role : "Organization"}
                </span>
              </div>
              <ChevronsUpDownIcon className="ml-auto size-4" />
            </SidebarMenuButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            className="w-(--radix-dropdown-menu-trigger-width) min-w-56 rounded-lg"
            side={isMobile ? "bottom" : "right"}
            align="start"
            sideOffset={4}
          >
            <DropdownMenuLabel className="text-xs text-muted-foreground">
              Organizations
            </DropdownMenuLabel>
            {memberships.map(({ organization }) => (
              <DropdownMenuItem
                key={organization.id}
                onClick={() => switchTo(organization.permalink)}
              >
                <div className="flex size-6 items-center justify-center rounded-md border text-[10px] font-semibold">
                  {orgInitials(organization.name)}
                </div>
                <span className="truncate">{organization.name}</span>
                {organization.permalink === activeOrg && (
                  <CheckIcon className="ml-auto size-4" />
                )}
              </DropdownMenuItem>
            ))}
            {memberships.length === 0 && (
              <p className="px-2 py-1.5 text-xs text-muted-foreground">
                No memberships yet.
              </p>
            )}
            {isAdmin && (
              <DropdownMenuItem onClick={() => router.push("/orgs")}>
                <ListIcon className="size-4" />
                <span className="text-muted-foreground">All organizations…</span>
              </DropdownMenuItem>
            )}
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={requestNewOrganization}>
              <PlusIcon className="size-4" /> Create organization
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    </SidebarMenu>
  )
}
