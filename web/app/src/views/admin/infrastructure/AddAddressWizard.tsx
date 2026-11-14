"use client"

// The "Add server or IP" wizard: enter the new address and the host name it
// will greet with, get the records to create, then check again until every
// record is in place. Read-only guidance: it never changes DNS or a pool.
// Reads GET /api/v2/admin/dns_checks/candidate, every 30 seconds while open
// and not ready yet.

import { useState } from "react"
import Link from "next/link"
import { useQuery } from "@tanstack/react-query"
import { CircleCheckIcon, RefreshCwIcon } from "lucide-react"
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert"
import { Button } from "@/components/ui/button"
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { relativeTime } from "@/lib/api-p1"
import {
  candidateProblems,
  infrastructureApi,
  instructionsText,
  sortGroups,
  type Candidate,
} from "@/lib/infrastructure"
import { CheckGroupCard, CopyAllButton } from "./checks-ui"

const EMPTY: Candidate = { ipv4: "", ipv6: "", hostname: "" }

export function AddAddressWizard({
  open,
  onOpenChange,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
}) {
  const [form, setForm] = useState<Candidate>(EMPTY)
  const [submitted, setSubmitted] = useState<Candidate | null>(null)
  const [touched, setTouched] = useState(false)
  const problems = candidateProblems(form)

  const run = useQuery({
    queryKey: ["admin", "dns-candidate", submitted],
    queryFn: () => infrastructureApi.candidate(submitted as Candidate),
    enabled: open && submitted !== null,
    // Keep checking while the operator creates the records.
    refetchInterval: (query) => (query.state.data?.ready ? false : 30_000),
  })

  function close(next: boolean) {
    if (!next) {
      setForm(EMPTY)
      setSubmitted(null)
      setTouched(false)
    }
    onOpenChange(next)
  }

  const data = run.data
  const groups = data ? sortGroups(data.groups) : []

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Add a server or an IP address</DialogTitle>
          <DialogDescription>
            Enter the new address and the host name it should carry. The wizard lists the records to
            create and checks them until every one is in place. It never changes DNS or a pool.
          </DialogDescription>
        </DialogHeader>

        {submitted === null ? (
          <form
            className="flex flex-col gap-4"
            onSubmit={(event) => {
              event.preventDefault()
              setTouched(true)
              if (problems.length === 0) setSubmitted({ ...form })
            }}
          >
            <div className="grid gap-4 sm:grid-cols-2">
              <div className="flex flex-col gap-2">
                <Label htmlFor="candidate-ipv4">IPv4 address</Label>
                <Input
                  id="candidate-ipv4"
                  placeholder="203.0.113.20"
                  value={form.ipv4}
                  onChange={(e) => setForm({ ...form, ipv4: e.target.value })}
                />
              </div>
              <div className="flex flex-col gap-2">
                <Label htmlFor="candidate-ipv6">IPv6 address (optional)</Label>
                <Input
                  id="candidate-ipv6"
                  placeholder="2001:db8::20"
                  value={form.ipv6}
                  onChange={(e) => setForm({ ...form, ipv6: e.target.value })}
                />
              </div>
            </div>
            <div className="flex flex-col gap-2">
              <Label htmlFor="candidate-hostname">Host name</Label>
              <Input
                id="candidate-hostname"
                placeholder="out2.mail.example.com"
                value={form.hostname}
                onChange={(e) => setForm({ ...form, hostname: e.target.value })}
              />
              <p className="text-xs text-muted-foreground">
                The name the address greets receivers with (EHLO). Its A record, the PTR record of the
                address and the EHLO name must all agree.
              </p>
            </div>
            {touched && problems.length > 0 && (
              <Alert variant="destructive">
                <AlertDescription>
                  {problems.map((problem) => (
                    <div key={problem}>{problem}</div>
                  ))}
                </AlertDescription>
              </Alert>
            )}
            <DialogFooter>
              <Button type="submit">Show the records</Button>
            </DialogFooter>
          </form>
        ) : (
          <div className="flex flex-col gap-4">
            {run.isError && (
              <Alert variant="destructive">
                <AlertTitle>The check could not be run</AlertTitle>
                <AlertDescription>{String(run.error)}</AlertDescription>
              </Alert>
            )}
            {data?.ready ? (
              <Alert>
                <CircleCheckIcon />
                <AlertTitle>Every record is in place</AlertTitle>
                <AlertDescription>
                  <p>
                    Add {[data.candidate.ipv4, data.candidate.ipv6].filter(Boolean).join(" / ")} to a pool
                    with the host name {data.candidate.hostname}, and set{" "}
                    <code>smtp_client.helo_from_ip_address: true</code> so the worker greets with it.
                  </p>
                  <Link href="/admin/ip-pools" className="mt-2 inline-block font-medium underline">
                    Open the IP pools
                  </Link>
                </AlertDescription>
              </Alert>
            ) : (
              data && (
                <div className="flex flex-col gap-2">
                  <h3 className="text-sm font-semibold">Records to create</h3>
                  <ol className="flex list-decimal flex-col gap-1 pl-5 text-sm">
                    {data.instructions.map((instruction) => (
                      <li key={instruction}>
                        <code className="text-xs break-all">{instruction}</code>
                      </li>
                    ))}
                  </ol>
                  <p className="text-xs text-muted-foreground">
                    The PTR record is set at the provider of the address (for Hetzner Cloud, in the
                    Primary or Floating IP settings), not in your DNS zone. Then add the address to a
                    pool with this host name and set <code>smtp_client.helo_from_ip_address: true</code>.
                  </p>
                </div>
              )
            )}
            <div className="flex flex-wrap items-center justify-between gap-2">
              <span className="text-sm text-muted-foreground">
                {run.isFetching
                  ? "Checking…"
                  : data
                    ? `Checked ${relativeTime(data.checked_at)}${data.ready ? "" : ", again every 30 seconds"}`
                    : null}
              </span>
              <div className="flex gap-2">
                <CopyAllButton text={instructionsText(groups)} label="Copy all" />
                <Button variant="outline" size="sm" onClick={() => run.refetch()} disabled={run.isFetching}>
                  <RefreshCwIcon className={run.isFetching ? "animate-spin" : undefined} />
                  Check again
                </Button>
              </div>
            </div>
            <div className="grid gap-4">
              {groups.map((group) => (
                <CheckGroupCard key={`${group.group}-${group.subject}`} group={group} />
              ))}
            </div>
            <DialogFooter>
              <Button variant="ghost" onClick={() => setSubmitted(null)}>
                Change the address
              </Button>
              <Button variant="outline" onClick={() => close(false)}>
                Close
              </Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  )
}
