"use client"

// Instance admin: the DNS checks. Every record the installation depends on,
// what DNS answers today, what it should answer, and the record to set,
// ready to copy. Reads GET /api/v2/admin/dns_checks; the "Add server or IP"
// wizard reads …/dns_checks/candidate. Nothing here changes DNS.

import { useState } from "react"
import { useQuery, useQueryClient } from "@tanstack/react-query"
import { PlusIcon, RefreshCwIcon } from "lucide-react"
import { EmptyState, PageHeader } from "@/components/shared"
import { Page } from "@/components/page"
import { StatusPill } from "@/components/status-pill"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import { relativeTime } from "@/lib/api-p1"
import { infrastructureApi, instructionsText, sortGroups } from "@/lib/infrastructure"
import { AddAddressWizard } from "./AddAddressWizard"
import { CheckGroupCard, CopyAllButton } from "./checks-ui"
import { InfrastructureNav } from "./InfrastructureNav"

export default function DnsChecks() {
  const queryClient = useQueryClient()
  const [refreshing, setRefreshing] = useState(false)
  const [wizardOpen, setWizardOpen] = useState(false)
  const checks = useQuery({
    queryKey: ["admin", "dns-checks"],
    queryFn: () => infrastructureApi.dnsChecks(false),
  })

  async function refresh() {
    setRefreshing(true)
    try {
      const fresh = await infrastructureApi.dnsChecks(true)
      queryClient.setQueryData(["admin", "dns-checks"], fresh)
    } finally {
      setRefreshing(false)
    }
  }

  const data = checks.data
  const groups = data ? sortGroups(data.groups) : []
  const allInstructions = instructionsText(groups)

  return (
    <Page
      header={
        <>
          <PageHeader
            title="Infrastructure"
            description="The DNS records this installation depends on, checked live. Copy the records to set; nothing here changes DNS."
            action={
              <>
                <Button variant="outline" size="sm" onClick={refresh} disabled={refreshing}>
                  <RefreshCwIcon className={refreshing ? "animate-spin" : undefined} />
                  Check again
                </Button>
                <Button size="sm" onClick={() => setWizardOpen(true)}>
                  <PlusIcon />
                  Add server or IP
                </Button>
              </>
            }
          />
          <InfrastructureNav />
        </>
      }
    >
      <div className="flex flex-col gap-6">
        {checks.isError && (
          <Alert variant="destructive">
            <AlertTitle>The DNS checks could not be run</AlertTitle>
            <AlertDescription>{String(checks.error)}</AlertDescription>
          </Alert>
        )}
        {data && (
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <StatusPill status={`${data.summary.fail} failed`} tone={data.summary.fail ? "red" : "gray"} />
              <StatusPill status={`${data.summary.warn} warnings`} tone={data.summary.warn ? "amber" : "gray"} />
              <StatusPill status={`${data.summary.ok} ok`} tone="green" />
              <span className="text-muted-foreground">Checked {relativeTime(data.checked_at)}</span>
            </div>
            <CopyAllButton text={allInstructions} label="Copy all records to set" />
          </div>
        )}
        {data && data.truncated_domains > 0 && (
          <p className="text-sm text-muted-foreground">
            {data.truncated_domains} more tenant domains are not checked here; the first 100 are.
          </p>
        )}
        {checks.isLoading && <EmptyState>Checking DNS…</EmptyState>}
        {data && groups.length === 0 && (
          <EmptyState>
            Nothing to check yet. Set cluster.inbound_hostname and add IP pool addresses, or let a worker
            register its public addresses.
          </EmptyState>
        )}
        <div className="grid gap-4 lg:grid-cols-2">
          {groups.map((group) => (
            <CheckGroupCard key={`${group.group}-${group.subject}`} group={group} />
          ))}
        </div>
      </div>
      <AddAddressWizard open={wizardOpen} onOpenChange={setWizardOpen} />
    </Page>
  )
}
