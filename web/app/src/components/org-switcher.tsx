"use client"

// The organization switcher lives in the sidebar (next to the server
// switcher). This module owns its shared pieces: the "new organization"
// event bridge, the initials helper, and the dropdown body
// (OrgSwitcherMenuContent) that lists all memberships (checkmark on the
// active one), "All organizations" for admins, and "Create organization".
// Picking an org navigates to its overview and remembers it in localStorage.

import { useRouter } from "next/navigation"
import { CheckIcon, ListIcon, PlusIcon } from "lucide-react"
import {
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
} from "@/components/ui/dropdown-menu"
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

// The dropdown body for the organization switcher: the full membership
// list (checkmark on the active org), "All organizations" (admins), and
// "Create organization". Rendered inside a <DropdownMenu>; the trigger is
// supplied by the caller so it can match its surroundings (the sidebar
// header's server switcher). Positioning defaults suit the sidebar.
export function OrgSwitcherMenuContent({
  activeOrg,
  side = "right",
  align = "start",
  sideOffset = 4,
  className = "w-(--radix-dropdown-menu-trigger-width) min-w-56 rounded-lg",
}: {
  activeOrg: string | undefined
  side?: "top" | "right" | "bottom" | "left"
  align?: "start" | "center" | "end"
  sideOffset?: number
  className?: string
}) {
  const { me } = useAuth()
  const router = useRouter()

  const memberships = me?.memberships ?? []
  const isAdmin = me?.user.admin ?? false

  function switchTo(permalink: string) {
    setLastActiveOrg(permalink)
    router.push(`/orgs/${permalink}`)
  }

  return (
    <DropdownMenuContent className={className} side={side} align={align} sideOffset={sideOffset}>
      <DropdownMenuLabel className="text-xs text-muted-foreground">
        Organizations
      </DropdownMenuLabel>
      {memberships.map(({ organization }) => (
        <DropdownMenuItem key={organization.id} onClick={() => switchTo(organization.permalink)}>
          <div className="flex size-6 items-center justify-center rounded-md border text-[10px] font-semibold">
            {orgInitials(organization.name)}
          </div>
          <span className="truncate">{organization.name}</span>
          {organization.permalink === activeOrg && <CheckIcon className="ml-auto size-4" />}
        </DropdownMenuItem>
      ))}
      {memberships.length === 0 && (
        <p className="px-2 py-1.5 text-xs text-muted-foreground">No memberships yet.</p>
      )}
      <DropdownMenuSeparator />
      {isAdmin && (
        <DropdownMenuItem onClick={() => router.push("/orgs")}>
          <ListIcon className="size-4" /> All organizations…
        </DropdownMenuItem>
      )}
      <DropdownMenuItem onClick={requestNewOrganization}>
        <PlusIcon className="size-4" /> Create organization
      </DropdownMenuItem>
    </DropdownMenuContent>
  )
}
