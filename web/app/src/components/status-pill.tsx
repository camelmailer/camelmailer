"use client"

// The one status-pill vocabulary of the app: a soft tinted pill with a
// dot, colored consistently everywhere — delivered green, bounced red,
// held amber, queued gray, opened teal, clicked violet. Verification
// and DNS-health states reuse the same tones (verified/ok green,
// warning amber, missing red, pending gray).
//
// Introduced in phase P2 ("trust"); the remaining views migrate from
// ad-hoc <Badge> usage in a later phase.

import { cn } from "@/lib/utils"
import type { Message } from "@/lib/api"

export type PillTone = "green" | "red" | "amber" | "gray" | "teal" | "violet"

const TONE_CLASSES: Record<PillTone, string> = {
  green: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-400",
  red: "bg-red-500/15 text-red-700 dark:text-red-400",
  amber: "bg-amber-500/15 text-amber-700 dark:text-amber-400",
  gray: "bg-muted text-muted-foreground",
  teal: "bg-teal-500/15 text-teal-700 dark:text-teal-400",
  violet: "bg-violet-500/15 text-violet-700 dark:text-violet-400",
}

// Lookup keys are lowercased with spaces/underscores/dashes stripped,
// so "Soft fail", "soft_fail" and "SoftFail" all resolve identically.
const STATUS_TONES: Record<string, PillTone> = {
  // green — it worked
  delivered: "green",
  sent: "green",
  verified: "green",
  ok: "green",
  confirmed: "green",
  ready: "green",
  passed: "green",
  active: "green",
  // red — it failed
  bounced: "red",
  hardfail: "red",
  missing: "red",
  failed: "red",
  rejected: "red",
  suppressed: "red",
  // amber — attention needed
  held: "amber",
  warning: "amber",
  softfail: "amber",
  delayed: "amber",
  nokey: "amber",
  // gray — nothing happened yet
  queued: "gray",
  pending: "gray",
  unverified: "gray",
  unchecked: "gray",
  // engagement
  opened: "teal",
  clicked: "violet",
}

export function statusTone(status: string): PillTone {
  return STATUS_TONES[status.toLowerCase().replace(/[\s_-]/g, "")] ?? "gray"
}

// Solid dot in the same tone — timeline markers etc.
const TONE_DOTS: Record<PillTone, string> = {
  green: "bg-emerald-500",
  red: "bg-red-500",
  amber: "bg-amber-500",
  gray: "bg-muted-foreground",
  teal: "bg-teal-500",
  violet: "bg-violet-500",
}

export function statusDotClass(status: string): string {
  return TONE_DOTS[statusTone(status)]
}

export function StatusPill({
  status,
  tone,
  className,
}: {
  /** Shown verbatim; also drives the color unless `tone` overrides it. */
  status: string
  tone?: PillTone
  className?: string
}) {
  return (
    <span
      className={cn(
        "inline-flex w-fit shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full px-2 py-0.5 text-xs font-medium",
        TONE_CLASSES[tone ?? statusTone(status)],
        className,
      )}
    >
      <span className="size-1.5 shrink-0 rounded-full bg-current opacity-70" aria-hidden />
      {status}
    </span>
  )
}

/// The display status of a message row (held wins over the raw status).
export function messageStatus(message: Pick<Message, "status" | "held">): string {
  if (message.held) return "held"
  switch (message.status) {
    case "Sent":
      return "delivered"
    case "SoftFail":
      return "soft fail"
    case "HardFail":
      return "hard fail"
    case "Bounced":
      return "bounced"
    case "Pending":
    case null:
    case undefined:
      return "queued"
    default:
      return message.status.toLowerCase()
  }
}

export function MessagePill({ message }: { message: Pick<Message, "status" | "held"> }) {
  return <StatusPill status={messageStatus(message)} />
}
