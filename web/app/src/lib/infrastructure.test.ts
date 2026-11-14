import { describe, expect, it } from "vitest"
import {
  candidateProblems,
  candidateQuery,
  hostRows,
  instructionsText,
  shortDuration,
  sortGroups,
  type ClusterNode,
  type DnsCheckGroup,
} from "./infrastructure"

function node(partial: Partial<ClusterNode>): ClusterNode {
  return {
    instance_id: "x",
    role: "worker",
    node_name: "mail-1",
    version: "0.15.0",
    status: "live",
    started_at: "2026-11-14T10:00:00Z",
    last_seen_at: "2026-11-14T10:00:00Z",
    seconds_since_seen: 5,
    stopped_at: null,
    details: {},
    leases: null,
    ...partial,
  }
}

const groups: DnsCheckGroup[] = [
  {
    group: "hostname",
    subject: "mail.example.com",
    status: "ok",
    checks: [
      { group: "hostname", subject: "mail.example.com", kind: "a", status: "ok", found: [], expected: [], instructions: [], detail: "" },
    ],
  },
  {
    group: "address",
    subject: "203.0.113.20",
    status: "fail",
    checks: [
      {
        group: "address",
        subject: "203.0.113.20",
        kind: "ptr",
        status: "fail",
        found: [],
        expected: ["out2.mail.example.com"],
        instructions: ["set PTR 203.0.113.20 → out2.mail.example.com (at the provider of the address)"],
        detail: "",
      },
      {
        group: "address",
        subject: "203.0.113.20",
        kind: "fcrdns",
        status: "fail",
        found: [],
        expected: [],
        instructions: ["set A out2.mail.example.com → 203.0.113.20, TTL 300"],
        detail: "",
      },
    ],
  },
  {
    group: "domain",
    subject: "tenant.example",
    status: "warn",
    checks: [
      {
        group: "domain",
        subject: "tenant.example",
        kind: "mx",
        status: "warn",
        found: [],
        expected: [],
        instructions: ["set A out2.mail.example.com → 203.0.113.20, TTL 300"],
        detail: "",
      },
    ],
  },
]

describe("DNS check helpers", () => {
  it("puts failures first and keeps the order within a status", () => {
    expect(sortGroups(groups).map((g) => g.subject)).toEqual([
      "203.0.113.20",
      "tenant.example",
      "mail.example.com",
    ])
  })

  it("copies every record to set once, one per line", () => {
    expect(instructionsText(groups)).toBe(
      "set PTR 203.0.113.20 → out2.mail.example.com (at the provider of the address)\n" +
        "set A out2.mail.example.com → 203.0.113.20, TTL 300",
    )
  })

  it("builds the candidate query without empty fields", () => {
    expect(candidateQuery({ ipv4: " 167.233.189.226 ", ipv6: "", hostname: "out2.mail.example.com" })).toBe(
      "ipv4=167.233.189.226&hostname=out2.mail.example.com",
    )
  })

  it("validates the wizard form before it asks the server", () => {
    expect(candidateProblems({ ipv4: "167.233.189.226", ipv6: "", hostname: "out2.mail.example.com" })).toEqual([])
    expect(candidateProblems({ ipv4: "", ipv6: "2001:db8::1", hostname: "out2.mail.example.com" })).toEqual([])
    expect(candidateProblems({ ipv4: "", ipv6: "", hostname: "out2.mail.example.com" })).toHaveLength(1)
    expect(candidateProblems({ ipv4: "300.1.1.1", ipv6: "", hostname: "out2" })).toHaveLength(2)
    expect(candidateProblems({ ipv4: "1.2.3.4", ipv6: "not-ip", hostname: "a.123" })).toHaveLength(2)
  })
})

describe("topology helpers", () => {
  it("folds processes into hosts", () => {
    const rows = hostRows([
      node({ instance_id: "w1", role: "worker", leases: { queued_messages: 3, webhook_requests: 1, lifecycle_events: 0, oldest_lock_at: null, oldest_lock_age_seconds: null }, details: { public_addresses: ["49.13.35.233"] } }),
      node({ instance_id: "s1", role: "smtp", version: "0.14.0", details: { public_addresses: ["49.13.35.233"] } }),
      node({ instance_id: "w-old", role: "worker", status: "stopped", leases: { queued_messages: 9, webhook_requests: 0, lifecycle_events: 0, oldest_lock_at: null, oldest_lock_age_seconds: null } }),
      node({ instance_id: "s2", role: "smtp", node_name: "mail-2", status: "stale", last_seen_at: "2026-11-14T09:00:00Z" }),
    ])
    expect(rows.map((r) => r.node_name)).toEqual(["mail-1", "mail-2"])
    expect(rows[0].roles).toEqual(["smtp", "worker"])
    expect(rows[0].versions.sort()).toEqual(["0.14.0", "0.15.0"])
    expect(rows[0].leases).toBe(4)
    expect(rows[0].public_addresses).toEqual(["49.13.35.233"])
    expect(rows[1].status).toBe("stale")
  })

  it("formats short durations", () => {
    expect(shortDuration(45)).toBe("45 s")
    expect(shortDuration(300)).toBe("5 min")
    expect(shortDuration(7200)).toBe("2 h")
    expect(shortDuration(null)).toBe("—")
  })
})
