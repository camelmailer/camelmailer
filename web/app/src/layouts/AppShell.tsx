// The signed-in application frame: sidebar with organizations + global
// admin sections, topbar with the account menu, content via <Outlet/>.

import { useState } from "react"
import {
  Link,
  NavLink,
  Outlet,
  useNavigate,
  useParams,
} from "react-router-dom"
import {
  BuildingIcon,
  ChevronDownIcon,
  KeyRoundIcon,
  NetworkIcon,
  PlusIcon,
  ScrollTextIcon,
  UsersIcon,
} from "lucide-react"
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
import { adminApi, ApiError } from "@/lib/api"
import { useAuth } from "@/lib/auth"
import { cn } from "@/lib/utils"
import { toast } from "sonner"

function SideLink({ to, children }: { to: string; children: React.ReactNode }) {
  return (
    <NavLink
      to={to}
      className={({ isActive }) =>
        cn(
          "flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-sidebar-foreground/80 hover:bg-sidebar-accent",
          isActive && "bg-sidebar-accent font-medium text-sidebar-foreground",
        )
      }
    >
      {children}
    </NavLink>
  )
}

export default function AppShell() {
  const { me, refresh, logout } = useAuth()
  const navigate = useNavigate()
  const { org } = useParams()
  const [newOrgOpen, setNewOrgOpen] = useState(false)
  const [newOrgName, setNewOrgName] = useState("")
  const [busy, setBusy] = useState(false)

  const memberships = me?.memberships ?? []
  const isAdmin = me?.user.admin ?? false

  async function createOrg() {
    setBusy(true)
    try {
      const { organization } = await adminApi.organizations.create(newOrgName)
      await refresh()
      setNewOrgOpen(false)
      setNewOrgName("")
      navigate(`/orgs/${organization.permalink}`)
    } catch (err) {
      toast.error(err instanceof ApiError ? err.message : "Could not create the organization")
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-svh">
      <aside className="flex w-60 shrink-0 flex-col border-r bg-sidebar">
        <div className="flex h-14 items-center border-b px-4">
          <Link to="/" className="font-semibold">
            CamelMailer 🐫
          </Link>
        </div>
        <nav className="flex-1 space-y-4 overflow-y-auto p-3">
          <div>
            <div className="mb-1 flex items-center justify-between px-2">
              <span className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
                Organizations
              </span>
              <Button
                variant="ghost"
                size="icon"
                className="size-5"
                onClick={() => setNewOrgOpen(true)}
                title="New organization"
              >
                <PlusIcon className="size-3.5" />
              </Button>
            </div>
            <div className="space-y-0.5">
              {memberships.map(({ organization, role }) => (
                <SideLink key={organization.id} to={`/orgs/${organization.permalink}`}>
                  <BuildingIcon className="size-4" />
                  <span className="truncate">{organization.name}</span>
                  <span className="ml-auto text-[10px] text-muted-foreground">{role}</span>
                </SideLink>
              ))}
              {isAdmin && (
                <SideLink to="/orgs">
                  <span className="pl-6 text-xs text-muted-foreground">All organizations…</span>
                </SideLink>
              )}
              {memberships.length === 0 && !isAdmin && (
                <p className="px-2 py-1 text-xs text-muted-foreground">No memberships yet.</p>
              )}
            </div>
          </div>
          {isAdmin && (
            <div>
              <div className="mb-1 px-2 text-xs font-medium uppercase tracking-wide text-muted-foreground">
                Instance admin
              </div>
              <div className="space-y-0.5">
                <SideLink to="/admin/users">
                  <UsersIcon className="size-4" /> Users
                </SideLink>
                <SideLink to="/admin/ip-pools">
                  <NetworkIcon className="size-4" /> IP pools
                </SideLink>
                <SideLink to="/admin/api-keys">
                  <KeyRoundIcon className="size-4" /> Admin API keys
                </SideLink>
                <SideLink to="/admin/audit">
                  <ScrollTextIcon className="size-4" /> Audit log
                </SideLink>
              </div>
            </div>
          )}
        </nav>
        <div className="border-t p-3">
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant="ghost" className="w-full justify-between">
                <span className="truncate text-sm">{me?.user.email_address}</span>
                <ChevronDownIcon className="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="start" className="w-52">
              <DropdownMenuLabel>
                {me?.user.first_name} {me?.user.last_name}
                {isAdmin && <span className="ml-1 text-xs text-muted-foreground">(admin)</span>}
              </DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => navigate("/account")}>
                Account & security
              </DropdownMenuItem>
              <DropdownMenuItem
                onClick={async () => {
                  await logout()
                  navigate("/login")
                }}
              >
                Sign out
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </aside>

      <main className="min-w-0 flex-1 p-6" key={org ?? "-"}>
        <Outlet />
      </main>

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
    </div>
  )
}
