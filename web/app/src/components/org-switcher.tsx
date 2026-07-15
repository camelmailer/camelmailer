"use client"

// The workspace-style organization switcher in the sidebar header:
// current org as initials avatar + name, dropdown with all memberships
// (checkmark on the active one), then "Create organization". Picking an
// org navigates to its overview and remembers it in localStorage.

import { useRouter } from "next/navigation"
import { CheckIcon, ChevronsUpDownIcon, ListIcon, PlusIcon } from "lucide-react"
import { Button, interactiveCard } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
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

// Compact, top-bar organization switcher, styled as a card to match the
// search / admin / account controls: org initials, name, and a caret,
// with the full membership list in a dropdown.
export function OrgSwitcher({ activeOrg }: { activeOrg: string | undefined }) {
  const { me } = useAuth()
  const router = useRouter()

  const memberships = me?.memberships ?? []
  const isAdmin = me?.user.admin ?? false
  const active = memberships.find((m) => m.organization.permalink === activeOrg)

  function switchTo(permalink: string) {
    setLastActiveOrg(permalink)
    router.push(`/orgs/${permalink}`)
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="sm" className={`gap-2 ${interactiveCard}`}>
          <div className="flex size-5 items-center justify-center rounded-md bg-primary text-[10px] font-semibold text-primary-foreground">
            {active ? orgInitials(active.organization.name) : "—"}
          </div>
          <span className="hidden max-w-40 truncate font-medium sm:inline">
            {active?.organization.name ?? activeOrg ?? "Select organization"}
          </span>
          <ChevronsUpDownIcon className="size-4 text-muted-foreground" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent className="min-w-56 rounded-lg" side="bottom" align="end" sideOffset={8}>
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
    </DropdownMenu>
  )
}
