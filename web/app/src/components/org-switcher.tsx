"use client"

// The organization switcher lives in the sidebar (below the server nav).
// This module owns the "new organization" event bridge and the switcher
// lightbox (OrgSwitcherDialog): a dialog holding the same organizations
// DataTable as the dashboard (name, role, server count, search), so you can
// scan and pick an organization to switch into. Its subheadline offers
// "create a new organization", which opens the create dialog. Global admins
// also get a link to the full "All organizations" view.

import { useRouter } from "next/navigation"
import { ListIcon } from "lucide-react"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { useAuth } from "@/lib/auth"
import { OrganizationsTable } from "@/views/dashboard-tables"

/// Ask the app shell to open the "New organization" dialog from
/// anywhere (empty states, command palette, the switcher lightbox) without
/// prop drilling.
export const NEW_ORG_EVENT = "camelmailer:new-organization"

export function requestNewOrganization() {
  if (typeof window !== "undefined") window.dispatchEvent(new Event(NEW_ORG_EVENT))
}

// The organization switcher lightbox. It reuses the dashboard's
// OrganizationsTable (fed the user's memberships) so the switcher and the
// overview stay visually identical. Picking a row navigates into that
// organization and closes the dialog.
export function OrgSwitcherDialog({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const { me } = useAuth()
  const router = useRouter()

  const isAdmin = me?.user.admin ?? false
  const orgs = (me?.memberships ?? []).map(({ organization, role }) => ({
    organization,
    role,
  }))

  function createNew() {
    onOpenChange(false)
    requestNewOrganization()
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Switch organization</DialogTitle>
          <DialogDescription>
            Jump to any organization you belong to, or{" "}
            <button
              type="button"
              onClick={createNew}
              className="font-medium text-foreground underline underline-offset-2 hover:text-primary"
            >
              create a new organization
            </button>
            .
          </DialogDescription>
        </DialogHeader>
        <OrganizationsTable orgs={orgs} onNavigate={() => onOpenChange(false)} />
        {isAdmin && (
          <button
            type="button"
            onClick={() => {
              onOpenChange(false)
              router.push("/orgs")
            }}
            className="flex items-center gap-2 self-start rounded-lg px-2 py-1.5 text-sm text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
          >
            <ListIcon className="size-4" /> All organizations…
          </button>
        )}
      </DialogContent>
    </Dialog>
  )
}
