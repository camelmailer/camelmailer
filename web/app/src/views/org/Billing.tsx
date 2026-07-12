"use client"

// Usage & Billing (masterplan §4.12). Two honest halves:
//   * Usage — real 30-day sent volume, summed across the org's servers
//     (never a fabricated limit or plan tier, since the API exposes none).
//   * Billing — only when the hosted `billing` group is enabled AND the
//     caller is admin/owner: a portal card that hands off to Stripe.
// Self-hosted installations (billing disabled) see usage alone.

import { useMutation, useQuery } from "@tanstack/react-query"
import { toast } from "sonner"
import { CreditCardIcon, SendIcon } from "lucide-react"
import { PageHeader } from "@/components/shared"
import { EmptyState } from "@/components/empty-state"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { adminApi, ApiError, type Role } from "@/lib/api"
import { canManageBilling, orgUsage } from "@/lib/api-p4"
import { useAuth } from "@/lib/auth"

function useOrgRole(org: string): Role | "root" | null {
  const { me } = useAuth()
  if (!me) return null
  if (me.user.admin) return "root"
  return me.memberships.find((m) => m.organization.permalink === org)?.role ?? null
}

export function BillingView({ org }: { org: string }) {
  const role = useOrgRole(org)
  const canBilling = canManageBilling(role)

  const usage = useQuery({
    queryKey: ["p4-org-usage", org],
    queryFn: () => orgUsage(org),
  })

  // 200 with enabled=false when billing is off (self-hosted) — never an error.
  const billing = useQuery({
    queryKey: ["billing", org],
    queryFn: () => adminApi.billing(org).get(),
    enabled: canBilling,
    retry: false,
  })
  const showBilling = canBilling && billing.data?.enabled === true

  const openPortal = useMutation({
    mutationFn: () => adminApi.billing(org).portal(),
    onSuccess: ({ url }) => {
      window.location.href = url
    },
    onError: (err) =>
      toast.error(
        err instanceof ApiError && err.code === "BillingUnavailable"
          ? "Billing is temporarily unavailable. Please try again in a few minutes."
          : err instanceof ApiError
            ? err.message
            : "Could not open the billing portal",
      ),
  })

  const u = usage.data
  const known = u?.perServer.filter((s) => s.sent != null) ?? []

  return (
    <div className="max-w-2xl space-y-6">
      <div>
        <PageHeader
          title="Usage"
          description="What this organization has sent recently."
        />
        <div className="grid gap-4 sm:grid-cols-2">
          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="flex items-center gap-2 text-sm text-muted-foreground">
                <SendIcon className="size-4" /> Sent (last 30 days)
              </CardTitle>
            </CardHeader>
            <CardContent>
              {usage.isLoading ? (
                <p className="text-2xl font-semibold text-muted-foreground">…</p>
              ) : u?.sent30d == null ? (
                <p className="text-sm text-muted-foreground">
                  Connect an API credential on a server to measure usage.
                </p>
              ) : (
                <p className="text-3xl font-semibold tabular-nums">
                  {u.sent30d.toLocaleString()}
                </p>
              )}
            </CardContent>
          </Card>

          {known.length > 1 && (
            <Card>
              <CardHeader className="pb-2">
                <CardTitle className="text-sm text-muted-foreground">By server</CardTitle>
              </CardHeader>
              <CardContent className="space-y-1.5 text-sm">
                {known.map((s) => (
                  <div key={s.permalink} className="flex items-center justify-between gap-2">
                    <span className="truncate text-muted-foreground">{s.name}</span>
                    <span className="tabular-nums font-medium">
                      {(s.sent ?? 0).toLocaleString()}
                    </span>
                  </div>
                ))}
              </CardContent>
            </Card>
          )}
        </div>
      </div>

      {showBilling && (
        <div>
          <PageHeader title="Billing" />
          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-base">
                <CreditCardIcon className="size-4" /> Billing portal
                {billing.data?.has_customer && (
                  <Badge variant="secondary" className="ml-1">
                    active customer
                  </Badge>
                )}
              </CardTitle>
            </CardHeader>
            <CardContent className="flex items-center justify-between gap-4">
              <p className="text-sm text-muted-foreground">
                Manage your subscription, payment methods and invoices in the secure
                billing portal.
              </p>
              <Button onClick={() => openPortal.mutate()} disabled={openPortal.isPending}>
                {openPortal.isPending ? "Opening…" : "Open portal"}
              </Button>
            </CardContent>
          </Card>
        </div>
      )}

      {canBilling && billing.isSuccess && !showBilling && (
        <EmptyState
          icon={CreditCardIcon}
          title="Billing is not enabled"
          description="This installation runs without the hosted billing add-on, so there is nothing to pay here. The usage above is for your own reference."
        />
      )}
    </div>
  )
}
