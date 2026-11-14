"use client"

// The pieces the DNS checks page and the add-address wizard share: status
// pills, a check with its records to set, a group card, a copy-all button.

import { useState } from "react"
import { CheckIcon, CopyIcon } from "lucide-react"
import { CopyButton } from "@/components/shared"
import { StatusPill } from "@/components/status-pill"
import { Button } from "@/components/ui/button"
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import {
  checkLabel,
  groupLabel,
  statusLabel,
  type CheckStatus,
  type DnsCheck,
  type DnsCheckGroup,
} from "@/lib/infrastructure"

const TONE = { ok: "green", warn: "amber", fail: "red" } as const

export function CheckStatusPill({ status }: { status: CheckStatus }) {
  return <StatusPill status={statusLabel(status)} tone={TONE[status]} />
}

/** Copies a block of text and shows that it did. */
export function CopyAllButton({ text, label }: { text: string; label: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <Button
      variant="outline"
      size="sm"
      disabled={!text}
      onClick={() => {
        navigator.clipboard.writeText(text)
        setCopied(true)
        setTimeout(() => setCopied(false), 1500)
      }}
    >
      {copied ? <CheckIcon /> : <CopyIcon />}
      {copied ? "Copied" : label}
    </Button>
  )
}

function Values({ label, values }: { label: string; values: string[] }) {
  return (
    <div className="min-w-0">
      <div className="text-xs text-muted-foreground">{label}</div>
      <div className="font-mono text-xs break-all">{values.length ? values.join(", ") : "—"}</div>
    </div>
  )
}

export function CheckRow({ check }: { check: DnsCheck }) {
  return (
    <div className="flex flex-col gap-2 border-t py-3 first:border-t-0 first:pt-0 last:pb-0">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="text-sm font-medium">{checkLabel(check.kind)}</span>
        <CheckStatusPill status={check.status} />
      </div>
      <p className="text-sm text-muted-foreground">{check.detail}</p>
      <div className="grid gap-2 sm:grid-cols-2">
        <Values label="Found" values={check.found} />
        <Values label="Expected" values={check.expected} />
      </div>
      {check.status !== "ok" &&
        check.instructions.map((instruction) => (
          <div key={instruction} className="flex items-center gap-2 rounded-md bg-muted px-2 py-1">
            <code className="min-w-0 flex-1 text-xs break-all">{instruction}</code>
            <CopyButton value={instruction} />
          </div>
        ))}
    </div>
  )
}

export function CheckGroupCard({ group }: { group: DnsCheckGroup }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center justify-between gap-2">
          <span className="min-w-0">
            <span className="block text-xs font-medium tracking-wider text-muted-foreground uppercase">
              {groupLabel(group.group)}
            </span>
            <span className="block truncate font-mono text-sm">{group.subject}</span>
          </span>
          <CheckStatusPill status={group.status} />
        </CardTitle>
      </CardHeader>
      <CardContent>
        {group.checks.map((check, index) => (
          <CheckRow key={`${check.kind}-${index}`} check={check} />
        ))}
      </CardContent>
    </Card>
  )
}
