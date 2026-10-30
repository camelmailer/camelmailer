"use client"

// Usage & billing for one organization.
//   * Usage: always. The server-side monthly counter across every server of
//     the organization (GET …/usage), with the monthly limit when the
//     installation sets one, and the suspension notice when there is one.
//   * Billing: only when a billing provider is active AND the caller is
//     admin/owner. Plans, checkout, subscription management and the
//     provider's own usage view are the provider's web components
//     (EmbedSession), with a plain link as fallback. The open-source build
//     has no provider, so self-hosted installations see usage only.

import { useState } from "react"
import { useMutation, useQuery } from "@tanstack/react-query"
import { toast } from "sonner"
import { ArchiveIcon, BanIcon, CreditCardIcon, PackageIcon, SendIcon } from "lucide-react"
import { PageHeader } from "@/components/shared"
import { EmbedSessionView } from "@/components/embed-session"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Skeleton } from "@/components/ui/skeleton"
import { cn } from "@/lib/utils"
import {
  adminApi,
  ApiError,
  type BillingPlan,
  type EmbedSession,
  type OrganizationRetention,
  type OrganizationUsage,
  type Role,
} from "@/lib/api"
import { canManageBilling, quotaPercent } from "@/lib/api-p4"
import { useAuth } from "@/lib/auth"

function useOrgRole(org: string): Role | "root" | null {
  const { me } = useAuth()
  if (!me) return null
  if (me.user.admin) return "root"
  return me.memberships.find((m) => m.organization.permalink === org)?.role ?? null
}

function billingError(err: unknown, fallback: string) {
  toast.error(
    err instanceof ApiError && err.code === "BillingUnavailable"
      ? "Billing is temporarily unavailable. Please try again in a few minutes."
      : err instanceof ApiError
        ? err.message
        : fallback,
  )
}

function formatMonth(month: string) {
  const [year, m] = month.split("-").map(Number)
  return new Date(Date.UTC(year, m - 1, 1)).toLocaleDateString(undefined, {
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  })
}

function formatPrice(plan: BillingPlan) {
  if (!plan.price) return null
  const amount = new Intl.NumberFormat(undefined, {
    style: "currency",
    currency: plan.price.currency,
  }).format(plan.price.amount_minor / 100)
  return plan.price.interval ? `${amount} / ${plan.price.interval}` : amount
}

/** The month's usage against the limit, or the plain count when unlimited. */
function QuotaMeter({ usage }: { usage: OrganizationUsage }) {
  const pct = quotaPercent(usage.sent, usage.limit)
  const resets = new Date(usage.period_end).toLocaleDateString(undefined, {
    day: "numeric",
    month: "long",
    timeZone: "UTC",
  })
  if (pct == null || usage.limit == null) {
    return (
      <div className="space-y-1">
        <p className="text-3xl font-semibold tabular-nums">{usage.sent.toLocaleString()}</p>
        <p className="text-xs text-muted-foreground">
          Sent in {formatMonth(usage.month)}. This organization has no monthly limit.
        </p>
      </div>
    )
  }
  const tone = pct >= 100 ? "bg-red-500" : pct >= 80 ? "bg-amber-500" : "bg-emerald-500"
  return (
    <div className="space-y-2">
      <div className="flex items-baseline justify-between text-sm">
        <span className="text-muted-foreground">Sent in {formatMonth(usage.month)}</span>
        <span className="tabular-nums font-medium">
          {usage.sent.toLocaleString()} / {usage.limit.toLocaleString()}
        </span>
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-muted">
        <div
          className={cn("h-full rounded-full transition-all", tone)}
          style={{ width: `${Math.max(2, pct)}%` }}
        />
      </div>
      <p className="text-xs text-muted-foreground">
        {pct >= 100
          ? `The monthly limit is reached, so sending is paused until ${resets}.`
          : `${(usage.remaining ?? 0).toLocaleString()} messages left. The count resets on ${resets} (UTC).`}
      </p>
    </div>
  )
}

function keptFor(days: number | null) {
  return days == null ? "kept until deleted" : `kept for ${days.toLocaleString()} days`
}

/** Read-only: how long this organization's data is kept. */
function RetentionCard({ retention }: { retention: OrganizationRetention }) {
  const rows: [string, number | null][] = [
    ["Message content", retention.message_content_days],
    ["Message history (status, deliveries, opens, clicks)", retention.message_metadata_days],
    ["Webhook delivery log", retention.webhook_log_days],
    ["DMARC reports", retention.dmarc_report_days],
  ]
  return (
    <Card>
      <CardHeader className="pb-2">
        <CardTitle className="flex items-center gap-2 text-sm text-muted-foreground">
          <ArchiveIcon className="size-4" /> Data retention
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-1.5 text-sm">
        {rows.map(([label, days]) => (
          <div key={label} className="flex items-center justify-between gap-4">
            <span className="text-muted-foreground">{label}</span>
            <span className="text-right">{keptFor(days)}</span>
          </div>
        ))}
      </CardContent>
    </Card>
  )
}

function BillingSection({ org, hasCustomer }: { org: string; hasCustomer: boolean }) {
  const [subscription, setSubscription] = useState<EmbedSession | null>(null)
  const [checkout, setCheckout] = useState<{ plan: BillingPlan; session: EmbedSession } | null>(
    null,
  )

  const plans = useQuery({
    queryKey: ["billing-plans", org],
    queryFn: () => adminApi.billing(org).plans(),
    retry: false,
  })
  // The provider's own usage view needs a customer; ask only once one
  // exists, so visiting the page never creates one.
  const providerUsage = useQuery({
    queryKey: ["billing-usage", org],
    queryFn: () => adminApi.billing(org).usage(),
    enabled: hasCustomer,
    retry: false,
  })

  const openPortal = useMutation({
    mutationFn: () => adminApi.billing(org).portal(),
    onSuccess: ({ session, url }) => {
      if (!session.element && url) {
        window.location.href = url
        return
      }
      setSubscription(session)
    },
    onError: (err) => billingError(err, "Could not open subscription management"),
  })

  const startCheckout = useMutation({
    mutationFn: (plan: BillingPlan) =>
      adminApi
        .billing(org)
        .checkout(plan.code)
        .then((result) => ({ plan, session: result.session })),
    onSuccess: setCheckout,
    onError: (err) => billingError(err, "Could not start the checkout"),
  })

  return (
    <div className="space-y-4">
      <PageHeader title="Billing" />

      {providerUsage.data?.session && (
        <Card>
          <CardContent className="pt-6">
            <EmbedSessionView session={providerUsage.data.session} linkLabel="View usage" />
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2 text-base">
            <CreditCardIcon className="size-4" /> Subscription
            {hasCustomer && (
              <Badge variant="secondary" className="ml-1">
                active customer
              </Badge>
            )}
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          {subscription ? (
            <EmbedSessionView session={subscription} linkLabel="Manage subscription" />
          ) : (
            <div className="flex items-center justify-between gap-4">
              <p className="text-sm text-muted-foreground">
                Your plan, payment method and invoices.
              </p>
              <Button onClick={() => openPortal.mutate()} disabled={openPortal.isPending}>
                {openPortal.isPending ? "Opening…" : "Manage subscription"}
              </Button>
            </div>
          )}
        </CardContent>
      </Card>

      {plans.isLoading ? (
        <Skeleton className="h-32 w-full" />
      ) : (plans.data?.plans.length ?? 0) > 0 ? (
        <div className="grid gap-4 sm:grid-cols-2">
          {plans.data!.plans.map((plan) => (
            <Card key={plan.code}>
              <CardHeader className="pb-2">
                <CardTitle className="flex items-center gap-2 text-base">
                  <PackageIcon className="size-4" /> {plan.name}
                </CardTitle>
              </CardHeader>
              <CardContent className="space-y-3">
                {formatPrice(plan) && (
                  <p className="text-2xl font-semibold tabular-nums">{formatPrice(plan)}</p>
                )}
                {plan.description && (
                  <p className="text-sm text-muted-foreground">{plan.description}</p>
                )}
                {plan.features.length > 0 && (
                  <ul className="space-y-1 text-sm text-muted-foreground">
                    {plan.features.map((feature) => (
                      <li key={feature}>{feature}</li>
                    ))}
                  </ul>
                )}
                <Button
                  variant="outline"
                  className="w-full"
                  onClick={() => startCheckout.mutate(plan)}
                  disabled={startCheckout.isPending}
                >
                  Choose {plan.name}
                </Button>
              </CardContent>
            </Card>
          ))}
        </div>
      ) : null}

      <Dialog open={!!checkout} onOpenChange={(open) => !open && setCheckout(null)}>
        <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
          <DialogHeader>
            <DialogTitle>{checkout ? `Checkout: ${checkout.plan.name}` : "Checkout"}</DialogTitle>
            <DialogDescription>Payment is handled by the billing provider.</DialogDescription>
          </DialogHeader>
          <EmbedSessionView session={checkout?.session} linkLabel="Continue to checkout" />
        </DialogContent>
      </Dialog>
    </div>
  )
}

export function BillingView({ org }: { org: string }) {
  const role = useOrgRole(org)
  const canBilling = canManageBilling(role)

  const usage = useQuery({
    queryKey: ["org-usage", org],
    queryFn: () => adminApi.usage(org),
  })

  // 200 with enabled=false when billing is off (self-hosted), never an error.
  const billing = useQuery({
    queryKey: ["billing", org],
    queryFn: () => adminApi.billing(org).get(),
    enabled: canBilling,
    retry: false,
  })
  const organization = useQuery({
    queryKey: ["organization", org, "retention"],
    queryFn: () => adminApi.organizations.get(org),
  })
  const showBilling = canBilling && billing.data?.enabled === true
  const u = usage.data?.usage
  const retention = organization.data?.retention

  return (
    <div className="max-w-2xl space-y-6">
      <div className="space-y-4">
        <PageHeader
          title="Usage"
          description="Messages sent this month across all servers of this organization."
        />
        {u?.suspended && (
          <Alert variant="destructive">
            <BanIcon />
            <AlertTitle>Sending is suspended</AlertTitle>
            <AlertDescription>
              {u.suspension_reason ?? "This organization has been suspended."} Contact the
              operator of this installation to lift the suspension.
            </AlertDescription>
          </Alert>
        )}
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="flex items-center gap-2 text-sm text-muted-foreground">
              <SendIcon className="size-4" /> This month
            </CardTitle>
          </CardHeader>
          <CardContent>
            {usage.isLoading ? (
              <Skeleton className="h-12 w-full" />
            ) : u ? (
              <QuotaMeter usage={u} />
            ) : (
              <p className="text-sm text-muted-foreground">Usage is unavailable right now.</p>
            )}
          </CardContent>
        </Card>
        {retention && <RetentionCard retention={retention} />}
      </div>

      {showBilling && <BillingSection org={org} hasCustomer={!!billing.data?.has_customer} />}
    </div>
  )
}
