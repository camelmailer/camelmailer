import { useQuery } from "@tanstack/react-query"
import { Link } from "react-router-dom"
import { BuildingIcon } from "lucide-react"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Badge } from "@/components/ui/badge"
import { EmptyState, PageHeader } from "@/components/shared"
import { adminApi } from "@/lib/api"
import { useAuth } from "@/lib/auth"

/// The landing page: the user's organizations. With `all` (global admins
/// via "All organizations…") every organization on the instance.
export default function Dashboard({ all = false }: { all?: boolean }) {
  const { me } = useAuth()
  const allOrgs = useQuery({
    queryKey: ["organizations", "all"],
    queryFn: adminApi.organizations.list,
    enabled: all,
  })

  const items = all
    ? (allOrgs.data?.organizations ?? []).map((organization) => ({
        organization,
        role: null as string | null,
      }))
    : (me?.memberships ?? []).map(({ organization, role }) => ({ organization, role }))

  return (
    <div>
      <PageHeader
        title={all ? "All organizations" : "Your organizations"}
        description={
          all
            ? "Every organization on this instance (global admin view)."
            : "Organizations you are a member of."
        }
      />
      {items.length === 0 ? (
        <EmptyState>
          No organizations yet. Create one with the + button in the sidebar.
        </EmptyState>
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {items.map(({ organization, role }) => (
            <Link key={organization.id} to={`/orgs/${organization.permalink}`}>
              <Card className="transition-colors hover:bg-accent/50">
                <CardHeader className="pb-2">
                  <CardTitle className="flex items-center gap-2 text-base">
                    <BuildingIcon className="size-4 text-muted-foreground" />
                    {organization.name}
                    {role && (
                      <Badge variant="secondary" className="ml-auto">
                        {role}
                      </Badge>
                    )}
                  </CardTitle>
                </CardHeader>
                <CardContent className="text-xs text-muted-foreground">
                  /{organization.permalink}
                </CardContent>
              </Card>
            </Link>
          ))}
        </div>
      )}
    </div>
  )
}
