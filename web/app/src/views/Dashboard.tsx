"use client"

import { useEffect } from "react"
import { useQueries, useQuery } from "@tanstack/react-query"
import Link from "next/link"
import { useRouter } from "next/navigation"
import { BuildingIcon } from "lucide-react"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { Badge } from "@/components/ui/badge"
import { PageHeader } from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { OnboardingChecklist } from "@/components/onboarding-checklist"
import { requestNewOrganization } from "@/components/org-switcher"
import { adminApi } from "@/lib/api"
import { getLastActiveOrg } from "@/lib/api-extras"
import { useAuth } from "@/lib/auth"

/// The landing page: the user's organizations. With `all` (global admins
/// via "All organizations…") every organization on the instance.
/// When the user has a remembered "last active" organization, /dashboard
/// forwards straight to its overview.
export default function Dashboard({ all = false }: { all?: boolean }) {
  const { me } = useAuth()
  const router = useRouter()
  const allOrgs = useQuery({
    queryKey: ["organizations", "all"],
    queryFn: adminApi.organizations.list,
    enabled: all,
  })

  // Forward to the last active organization, if it still exists.
  useEffect(() => {
    if (all || !me) return
    const last = getLastActiveOrg()
    if (last && me.memberships.some((m) => m.organization.permalink === last)) {
      router.replace(`/orgs/${last}`)
    }
  }, [all, me, router])

  const items = all
    ? (allOrgs.data?.organizations ?? []).map((organization) => ({
        organization,
        role: null as string | null,
      }))
    : (me?.memberships ?? []).map(({ organization, role }) => ({ organization, role }))

  // Server counts across the user's organizations (shares the sidebar's
  // per-org query). Anything we cannot fetch shows as "—" — no made-up
  // numbers.
  const serverQueries = useQueries({
    queries: (all ? [] : items).map(({ organization }) => ({
      queryKey: ["servers", organization.permalink],
      queryFn: () => adminApi.servers(organization.permalink).list(),
    })),
  })
  const serverCount =
    !all && serverQueries.every((q) => q.isSuccess)
      ? serverQueries.reduce((n, q) => n + (q.data?.servers.length ?? 0), 0)
      : null

  const defaultOrg = all ? undefined : items[0]?.organization.permalink

  return (
    <div>
      {!all && defaultOrg && <OnboardingChecklist org={defaultOrg} />}
      {!all && (
        <div className="mb-6 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <Card className="gap-2 py-5">
            <CardHeader>
              <CardDescription>Organizations</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums">
                {items.length}
              </CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Organizations you are a member of
            </CardContent>
          </Card>
          <Card className="gap-2 py-5">
            <CardHeader>
              <CardDescription>Servers</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums">
                {serverCount ?? "—"}
              </CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Mail servers across your organizations
            </CardContent>
          </Card>
          <Card className="gap-2 py-5">
            <CardHeader>
              <CardDescription>Messages (30d)</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums">—</CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Available per server under Messaging → Statistics
            </CardContent>
          </Card>
          <Card className="gap-2 py-5">
            <CardHeader>
              <CardDescription>Delivery rate</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums">—</CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Available per server under Messaging → Statistics
            </CardContent>
          </Card>
        </div>
      )}
      <PageHeader
        title={all ? "All organizations" : "Your organizations"}
        description={
          all
            ? "Every organization on this instance (global admin view)."
            : "Organizations you are a member of."
        }
      />
      {items.length === 0 ? (
        <EmptyState
          icon={BuildingIcon}
          title="No organizations yet"
          description="An organization groups your mail servers, domains and team in one place."
          action={{ label: "Create organization", onClick: requestNewOrganization }}
        />
      ) : (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {items.map(({ organization, role }) => (
            <Link key={organization.id} href={`/orgs/${organization.permalink}`}>
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
