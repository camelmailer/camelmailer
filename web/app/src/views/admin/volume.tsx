"use client"

// Shared reading of the administration overview endpoints: how traffic is
// formatted, which window is on screen, and how a tenant's numbers turn
// into an activity state plus the flags an operator should look at.
//
// The API reports counters only. Every judgment lives here, in one place,
// so the organization list, the organization detail page and the
// attention strip all agree.

import { type PillTone } from "@/components/status-pill"
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs"
import type { VolumeCounters, VolumeRow, VolumeWindow } from "@/lib/api"

export const WINDOWS: { value: VolumeWindow; label: string; long: string }[] = [
  { value: "day", label: "24h", long: "the last 24 hours" },
  { value: "week", label: "7d", long: "the last 7 days" },
  { value: "month", label: "30d", long: "the last 30 days" },
]

export function windowLabel(window: VolumeWindow): string {
  return WINDOWS.find((w) => w.value === window)?.label ?? window
}

export function windowDescription(window: VolumeWindow): string {
  return WINDOWS.find((w) => w.value === window)?.long ?? window
}

/// The window switch, small enough to sit in a table toolbar.
export function WindowToggle({
  value,
  onChange,
}: {
  value: VolumeWindow
  onChange: (value: VolumeWindow) => void
}) {
  return (
    <Tabs value={value} onValueChange={(next) => onChange(next as VolumeWindow)}>
      <TabsList>
        {WINDOWS.map((w) => (
          <TabsTrigger key={w.value} value={w.value} className="px-3">
            {w.label}
          </TabsTrigger>
        ))}
      </TabsList>
    </Tabs>
  )
}

/// Thousands separators, and the no-data placeholder for a real zero so a
/// quiet row reads as quiet instead of as a number.
export function count(value: number): string {
  return value === 0 ? "—" : value.toLocaleString("en-US")
}

/// "1 message" / "4 messages", for the sentences the attention strip
/// builds out of counters.
export function plural(value: number, singular: string, plural = `${singular}s`): string {
  return `${value.toLocaleString("en-US")} ${value === 1 ? singular : plural}`
}

/// Bounced share of the window's **outgoing** messages, or "—" while
/// there is nothing to divide by. A bounce is an outgoing-mail outcome, so
/// dividing by the total would let a busy inbound stream dilute the number
/// in the direction that hides a problem.
export function bounceRate(counters: VolumeCounters): string {
  const rate = bounceShare(counters)
  return rate === null ? "—" : `${(rate * 100).toFixed(1)}%`
}

/// The same share as a number, for sorting and for the flag threshold.
/// `null` when the window holds no outgoing mail.
export function bounceShare(counters: VolumeCounters): number | null {
  if (counters.outgoing === 0) return null
  return counters.bounced / counters.outgoing
}

/// Below this many outgoing messages a bounce share says more about the
/// sample than about the sender, so the bounce flag stays quiet.
const BOUNCE_FLOOR = 20
/// The share of bounces that is worth a second look. Matches the
/// deliverability rule the servers table already applies.
const BOUNCE_LIMIT = 0.1

/// What a tenant with servers is doing right now. Suspension outranks
/// everything: it is a decision somebody made about this tenant.
export function activity(row: {
  servers: number
  suspended_servers: number
  day: VolumeCounters
  month: VolumeCounters
}): { label: string; tone: PillTone } {
  if (row.servers > 0 && row.suspended_servers === row.servers) {
    return { label: "Suspended", tone: "red" }
  }
  if (row.day.total > 0) return { label: "Sending", tone: "green" }
  if (row.month.total > 0) return { label: "Quiet", tone: "gray" }
  return { label: "Idle", tone: "gray" }
}

export const ACTIVITY_STATES = ["Sending", "Quiet", "Idle", "Suspended"] as const

/// One thing about a tenant worth an operator's attention, with the
/// sentence the attention strip shows.
export type Flag = {
  label: string
  tone: PillTone
  reason: string
}

export const FLAG_LABELS = ["Newly active", "Held mail", "Check bounces", "Failing"] as const

/// The flags a tenant carries, judged over the 30-day window. Composable
/// by design: a brand-new sender with a bounce problem shows both.
export function flags(row: VolumeRow, now: number = Date.now()): Flag[] {
  const found: Flag[] = []
  // `first_message_at` is bounded by the widest window, so a tenant that
  // was silent for 30 days and resumed looks brand new. Both cases are
  // worth a look, and the label says only what is known.
  const first = row.first_message_at ? new Date(row.first_message_at).getTime() : null
  if (first !== null && now - first < 24 * 60 * 60 * 1000 && row.day.total > 0) {
    found.push({
      label: "Newly active",
      tone: "teal",
      reason: `${plural(row.day.total, "message")} in the last 24 hours, with nothing before them in 30 days`,
    })
  }
  const share = bounceShare(row.month)
  if (row.month.outgoing >= BOUNCE_FLOOR && share !== null && share > BOUNCE_LIMIT) {
    found.push({
      label: "Check bounces",
      tone: "red",
      reason: `${bounceRate(row.month)} of 30-day outgoing mail bounced`,
    })
  }
  if (row.month.held > 0) {
    found.push({
      label: "Held mail",
      tone: "amber",
      reason: `${plural(row.month.held, "message")} held in 30 days`,
    })
  }
  if (row.month.failed > 0) {
    found.push({
      label: "Failing",
      tone: "amber",
      reason: `${plural(row.month.failed, "delivery failure")} in 30 days`,
    })
  }
  return found
}
