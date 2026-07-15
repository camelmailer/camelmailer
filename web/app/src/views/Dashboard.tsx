"use client"

import { useQueries, useQuery } from "@tanstack/react-query"
import { BuildingIcon } from "lucide-react"
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card"
import { PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { EmptyState } from "@/components/empty-state"
import { OnboardingChecklist } from "@/components/onboarding-checklist"
import { requestNewOrganization } from "@/components/org-switcher"
import { adminApi } from "@/lib/api"
import { useAuth } from "@/lib/auth"
import { OrganizationsTable, ServersTable } from "./dashboard-tables"

/// The landing page ("/dashboard"): the signed-in user's overview — KPI
/// tiles, the onboarding checklist, and the user's organizations. With
/// `all` (global admins via "All organizations…") it lists every
/// organization on the instance. It always renders here; picking an org
/// from the switcher is what navigates into an organization.
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
    <Page
      variant="scroll"
      header={
        <PageHeader
          title={all ? "All organizations" : "Organizations"}
          description={
            all
              ? "Every organization on this instance (global admin view)."
              : "Organizations you are a member of."
          }
          className="mb-0"
        />
      }
    >
      {!all && defaultOrg && <OnboardingChecklist org={defaultOrg} />}
      {!all && (
        <div className="mb-6 grid gap-4 sm:grid-cols-2 lg:grid-cols-4 *:data-[slot=card]:from-primary/5 *:data-[slot=card]:to-card *:data-[slot=card]:bg-gradient-to-t *:data-[slot=card]:shadow-xs">
          <Card className="@container/card gap-2 py-5">
            <CardHeader>
              <CardDescription>Organizations</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums @[180px]/card:text-3xl">
                {items.length}
              </CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Organizations you are a member of
            </CardContent>
          </Card>
          <Card className="@container/card gap-2 py-5">
            <CardHeader>
              <CardDescription>Servers</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums @[180px]/card:text-3xl">
                {serverCount ?? "—"}
              </CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Mail servers across your organizations
            </CardContent>
          </Card>
          <Card className="@container/card gap-2 py-5">
            <CardHeader>
              <CardDescription>Messages (30d)</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums @[180px]/card:text-3xl">
                —
              </CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Available per server under Messaging → Statistics
            </CardContent>
          </Card>
          <Card className="@container/card gap-2 py-5">
            <CardHeader>
              <CardDescription>Delivery rate</CardDescription>
              <CardTitle className="text-2xl font-semibold tabular-nums @[180px]/card:text-3xl">
                —
              </CardTitle>
            </CardHeader>
            <CardContent className="text-xs text-muted-foreground">
              Available per server under Messaging → Statistics
            </CardContent>
          </Card>
        </div>
      )}
      {items.length === 0 ? (
        <EmptyState
          icon={BuildingIcon}
          title="No organizations yet"
          description="An organization groups your mail servers, domains and team in one place."
          action={{ label: "Create organization", onClick: requestNewOrganization }}
        />
      ) : (
        <OrganizationsTable orgs={items} />
      )}

      {!all && items.length > 0 && (
        <div className="mt-8">
          <PageHeader
            title="Servers"
            description="Mail servers across your organizations, with 30-day activity."
          />
          <ServersTable orgs={items} />
        </div>
      )}
    </Page>
  )
}
