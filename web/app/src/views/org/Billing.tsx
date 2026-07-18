"use client"

// Usage & Billing (masterplan §4.12). Three honest sections:
//   * Usage — real 30-day sent volume, summed across the org's servers
//     (never a fabricated limit or plan tier, since the API exposes none).
//   * Plan — the cloud plan/quota model: the public-beta cap, the upcoming
//     Base package, and the over-quota choice (auto-upgrade vs buy packages).
//     This is informational and renders whether or not the billing backend
//     is enabled (during the public beta there is nothing to charge yet).
//   * Billing — only when the hosted `billing` group is enabled AND the
//     caller is admin/owner: a portal card that hands off to Stripe.
// Self-hosted installations (billing disabled) see usage + the plan preview.

import { useState } from "react"
import { useMutation, useQuery } from "@tanstack/react-query"
import { toast } from "sonner"
import {
  CreditCardIcon,
  SendIcon,
  SparklesIcon,
  ZapIcon,
  PackageIcon,
  LineChartIcon,
} from "lucide-react"
import { PageHeader } from "@/components/shared"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import { adminApi, ApiError, type Role } from "@/lib/api"
import { canManageBilling, orgUsage } from "@/lib/api-p4"
import { useAuth } from "@/lib/auth"

// Public-beta sending cap and the planned Base package, in one place.
const BETA_MONTHLY_CAP = 5000
const PACKAGE_SIZE = 5000
const PACKAGE_PRICE = "€5"

type OverageChoice = "auto-upgrade" | "buy-packages"

function useOrgRole(org: string): Role | "root" | null {
  const { me } = useAuth()
  if (!me) return null
  if (me.user.admin) return "root"
  return me.memberships.find((m) => m.organization.permalink === org)?.role ?? null
}

/** A quota meter for the public-beta cap. Uses the real 30-day sent figure
 *  as the best available signal (the API exposes no per-calendar-month
 *  counter yet), and is honest about that in its label. */
function QuotaMeter({ sent }: { sent: number }) {
  const pct = Math.min(100, (sent / BETA_MONTHLY_CAP) * 100)
  const tone =
    pct >= 100
      ? "bg-red-500"
      : pct >= 80
        ? "bg-amber-500"
        : "bg-emerald-500"
  return (
    <div className="space-y-2">
      <div className="flex items-baseline justify-between text-sm">
        <span className="text-muted-foreground">Sent (last 30 days)</span>
        <span className="tabular-nums font-medium">
          {sent.toLocaleString()} / {BETA_MONTHLY_CAP.toLocaleString()}
        </span>
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-muted">
        <div
          className={cn("h-full rounded-full transition-all", tone)}
          style={{ width: `${Math.max(2, pct)}%` }}
        />
      </div>
      <p className="text-xs text-muted-foreground">
        Public beta cap is {BETA_MONTHLY_CAP.toLocaleString()} emails per
        calendar month.
      </p>
    </div>
  )
}

export function BillingView({ org }: { org: string }) {
  const role = useOrgRole(org)
  const canBilling = canManageBilling(role)
  const [overage, setOverage] = useState<OverageChoice>("auto-upgrade")

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
                <Skeleton className="h-9 w-24" />
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

      <div>
        <PageHeader
          title="Plan"
          description="The cloud plan and quota. Pricing launches after the public beta."
        />

        <div className="space-y-4">
          <Alert>
            <SparklesIcon />
            <AlertTitle>
              Public beta — {BETA_MONTHLY_CAP.toLocaleString()} emails per month
            </AlertTitle>
            <AlertDescription>
              <p>
                Sending is free while CamelMailer is in public beta, up to{" "}
                {BETA_MONTHLY_CAP.toLocaleString()} emails per calendar month.
                Paid plans launch soon, and you will be able to review them here
                before anything is charged.
              </p>
            </AlertDescription>
          </Alert>

          <Card>
            <CardHeader className="pb-2">
              <CardTitle className="text-sm text-muted-foreground">
                Monthly quota
              </CardTitle>
            </CardHeader>
            <CardContent>
              {usage.isLoading ? (
                <Skeleton className="h-12 w-full" />
              ) : u?.sent30d == null ? (
                <p className="text-sm text-muted-foreground">
                  Connect an API credential on a server to track usage against
                  your quota.
                </p>
              ) : (
                <QuotaMeter sent={u.sent30d} />
              )}
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle className="flex items-center gap-2 text-base">
                <PackageIcon className="size-4" /> Base package
                <Badge variant="secondary" className="ml-1">
                  Coming soon
                </Badge>
              </CardTitle>
            </CardHeader>
            <CardContent className="space-y-3">
              <div className="flex items-baseline gap-2">
                <span className="text-3xl font-semibold tabular-nums">
                  {PACKAGE_PRICE}
                </span>
                <span className="text-sm text-muted-foreground">
                  / month for {PACKAGE_SIZE.toLocaleString()} emails
                </span>
              </div>
              <ul className="space-y-1.5 text-sm text-muted-foreground">
                <li className="flex items-center gap-2">
                  <SendIcon className="size-4 shrink-0" />
                  {PACKAGE_SIZE.toLocaleString()} emails per calendar month,
                  billed monthly.
                </li>
                <li className="flex items-center gap-2">
                  <LineChartIcon className="size-4 shrink-0" />
                  Open and click tracking, shown right here in the app.
                </li>
                <li className="flex items-center gap-2">
                  <SparklesIcon className="size-4 shrink-0" />
                  Cloud offering, hosted on EU infrastructure.
                </li>
              </ul>
            </CardContent>
          </Card>

          <Card>
            <CardHeader className="pb-3">
              <CardTitle className="text-base">When you pass your quota</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <div className="grid gap-2 sm:grid-cols-2">
                <button
                  type="button"
                  onClick={() => setOverage("auto-upgrade")}
                  aria-pressed={overage === "auto-upgrade"}
                  className={cn(
                    "flex flex-col items-start gap-1 rounded-lg border p-3 text-left transition-colors",
                    overage === "auto-upgrade"
                      ? "border-primary bg-primary/5 ring-1 ring-primary"
                      : "hover:bg-accent",
                  )}
                >
                  <span className="flex items-center gap-2 text-sm font-medium">
                    <ZapIcon className="size-4" /> Auto-upgrade
                  </span>
                  <span className="text-xs text-muted-foreground">
                    Keep sending past your quota. The extra volume is billed
                    automatically at the {PACKAGE_PRICE} /{" "}
                    {PACKAGE_SIZE.toLocaleString()} rate.
                  </span>
                </button>

                <button
                  type="button"
                  onClick={() => setOverage("buy-packages")}
                  aria-pressed={overage === "buy-packages"}
                  className={cn(
                    "flex flex-col items-start gap-1 rounded-lg border p-3 text-left transition-colors",
                    overage === "buy-packages"
                      ? "border-primary bg-primary/5 ring-1 ring-primary"
                      : "hover:bg-accent",
                  )}
                >
                  <span className="flex items-center gap-2 text-sm font-medium">
                    <PackageIcon className="size-4" /> Buy packages
                  </span>
                  <span className="text-xs text-muted-foreground">
                    Add blocks of {PACKAGE_SIZE.toLocaleString()} emails at{" "}
                    {PACKAGE_PRICE} each, so a month stays within a fixed budget.
                  </span>
                </button>
              </div>
              <p className="text-xs text-muted-foreground">
                This choice is a preview of how over-quota sending will work.
                You can set it for real once paid plans launch.
              </p>
            </CardContent>
          </Card>
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
    </div>
  )
}
