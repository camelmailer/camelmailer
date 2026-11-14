// The infrastructure area of the administration: cluster topology
// (GET /api/v2/admin/cluster) and the DNS checks with the records to set
// (GET /api/v2/admin/dns_checks, …/dns_checks/candidate). Types, the API
// calls and the pure helpers the pages use; the helpers are unit-tested in
// infrastructure.test.ts.

import { api } from "@/lib/api"

// ------------------------------------------------------------------ types

export type NodeRole = "web" | "smtp" | "worker"
export type NodeStatus = "live" | "stale" | "stopped"

export type NodeLeases = {
  queued_messages: number
  webhook_requests: number
  lifecycle_events: number
  oldest_lock_at: string | null
  oldest_lock_age_seconds: number | null
}

export type ClusterNode = {
  instance_id: string
  role: NodeRole
  node_name: string
  version: string
  status: NodeStatus
  started_at: string
  last_seen_at: string
  seconds_since_seen: number
  stopped_at: string | null
  details: Record<string, unknown>
  leases: NodeLeases | null
}

export type ClusterPoolAddress = {
  id: number
  ipv4: string
  ipv6: string | null
  hostname: string
  priority: number
}

export type ClusterPool = {
  id: number
  name: string
  default: boolean
  address_family: string | null
  addresses: ClusterPoolAddress[]
}

export type Cluster = {
  cluster: {
    load_balancer_address: string | null
    inbound_hostname: string | null
    heartbeat_interval_seconds: number
    node_stale_seconds: number
  }
  nodes: ClusterNode[]
  unregistered_leases: (NodeLeases & { locked_by: string })[]
  versions: string[]
  mixed_versions: boolean
  ip_pools: ClusterPool[]
}

export type CheckStatus = "ok" | "warn" | "fail"

export type DnsCheck = {
  group: "address" | "hostname" | "domain"
  subject: string
  kind: "a" | "ptr" | "fcrdns" | "ehlo" | "spf" | "dkim" | "dmarc" | "mx" | "resolves"
  status: CheckStatus
  found: string[]
  expected: string[]
  instructions: string[]
  detail: string
}

export type DnsCheckGroup = {
  group: DnsCheck["group"]
  subject: string
  status: CheckStatus
  checks: DnsCheck[]
}

export type DnsCheckRun = {
  summary: Record<CheckStatus, number>
  groups: DnsCheckGroup[]
  instructions: string[]
  truncated_domains: number
  checked_at: string
  cached?: boolean
}

export type CandidateRun = DnsCheckRun & {
  ready: boolean
  candidate: { ipv4: string | null; ipv6: string | null; hostname: string }
}

export type Candidate = { ipv4: string; ipv6: string; hostname: string }

// -------------------------------------------------------------------- api

export const infrastructureApi = {
  cluster: () => api.get<Cluster>("/api/v2/admin/cluster"),
  dnsChecks: (refresh = false) =>
    api.get<DnsCheckRun>(`/api/v2/admin/dns_checks${refresh ? "?refresh=1" : ""}`),
  candidate: (candidate: Candidate) =>
    api.get<CandidateRun>(`/api/v2/admin/dns_checks/candidate?${candidateQuery(candidate)}`),
}

// ---------------------------------------------------------------- helpers

/** The query string of a candidate check; empty fields are left out. */
export function candidateQuery(candidate: Candidate): string {
  const params = new URLSearchParams()
  for (const key of ["ipv4", "ipv6", "hostname"] as const) {
    const value = candidate[key].trim()
    if (value) params.set(key, value)
  }
  return params.toString()
}

const CHECK_LABELS: Record<DnsCheck["kind"], string> = {
  a: "A/AAAA",
  ptr: "PTR",
  fcrdns: "Forward-confirmed rDNS",
  ehlo: "EHLO name = PTR name",
  spf: "SPF",
  dkim: "DKIM",
  dmarc: "DMARC",
  mx: "MX",
  resolves: "Resolves",
}

export function checkLabel(kind: DnsCheck["kind"]): string {
  return CHECK_LABELS[kind] ?? kind
}

/** The pill label of a check status. */
export function statusLabel(status: CheckStatus): string {
  return status === "ok" ? "ok" : status === "warn" ? "warning" : "failed"
}

const RANK: Record<CheckStatus, number> = { fail: 0, warn: 1, ok: 2 }

/** Groups failures first, then warnings, keeping the server's order within. */
export function sortGroups(groups: DnsCheckGroup[]): DnsCheckGroup[] {
  return groups
    .map((group, index) => ({ group, index }))
    .sort((a, b) => RANK[a.group.status] - RANK[b.group.status] || a.index - b.index)
    .map(({ group }) => group)
}

/** Every instruction of the checks that do not pass, each once, one per
 *  line: what "Copy all" puts on the clipboard. */
export function instructionsText(groups: DnsCheckGroup[]): string {
  const seen: string[] = []
  for (const group of groups) {
    for (const check of group.checks) {
      if (check.status === "ok") continue
      for (const instruction of check.instructions) {
        if (!seen.includes(instruction)) seen.push(instruction)
      }
    }
  }
  return seen.join("\n")
}

/** "203.0.113.10" → "Address", "mail.example.com" → "Host name", … */
export function groupLabel(group: DnsCheckGroup["group"]): string {
  return group === "address" ? "Sending address" : group === "hostname" ? "Host name" : "Domain"
}

/** One row per host: the processes registered under each node name. */
export type HostRow = {
  node_name: string
  roles: NodeRole[]
  versions: string[]
  status: NodeStatus
  last_seen_at: string
  public_addresses: string[]
  leases: number
  nodes: ClusterNode[]
}

const STATUS_RANK: Record<NodeStatus, number> = { live: 0, stale: 1, stopped: 2 }

/** Fold the processes into hosts. A host is live when any process on it is
 *  live; leases add up over its workers. */
export function hostRows(nodes: ClusterNode[]): HostRow[] {
  const hosts = new Map<string, HostRow>()
  for (const node of nodes) {
    const host = hosts.get(node.node_name) ?? {
      node_name: node.node_name,
      roles: [],
      versions: [],
      status: "stopped" as NodeStatus,
      last_seen_at: node.last_seen_at,
      public_addresses: [],
      leases: 0,
      nodes: [],
    }
    host.nodes.push(node)
    if (node.status !== "stopped" && !host.roles.includes(node.role)) host.roles.push(node.role)
    if (node.status === "live" && !host.versions.includes(node.version)) host.versions.push(node.version)
    if (STATUS_RANK[node.status] < STATUS_RANK[host.status]) host.status = node.status
    if (node.last_seen_at > host.last_seen_at) host.last_seen_at = node.last_seen_at
    const addresses = node.details?.public_addresses
    if (Array.isArray(addresses)) {
      for (const address of addresses) {
        if (typeof address === "string" && !host.public_addresses.includes(address)) {
          host.public_addresses.push(address)
        }
      }
    }
    if (node.leases && node.status !== "stopped") {
      host.leases +=
        node.leases.queued_messages + node.leases.webhook_requests + node.leases.lifecycle_events
    }
    hosts.set(node.node_name, host)
  }
  const roleOrder: NodeRole[] = ["smtp", "web", "worker"]
  return [...hosts.values()]
    .map((host) => ({
      ...host,
      roles: roleOrder.filter((role) => host.roles.includes(role)),
    }))
    .sort((a, b) => STATUS_RANK[a.status] - STATUS_RANK[b.status] || a.node_name.localeCompare(b.node_name))
}

/** Seconds as "45 s", "5 min", "2 h". */
export function shortDuration(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined) return "—"
  if (seconds < 90) return `${seconds} s`
  if (seconds < 90 * 60) return `${Math.round(seconds / 60)} min`
  return `${Math.round(seconds / 3600)} h`
}

const IPV4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/
const LABEL = /^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$/i

/** Light client-side validation of the wizard form; the server validates
 *  again. Returns the problems, empty when the form can be checked. */
export function candidateProblems(candidate: Candidate): string[] {
  const problems: string[] = []
  const ipv4 = candidate.ipv4.trim()
  const ipv6 = candidate.ipv6.trim()
  const hostname = candidate.hostname.trim()
  if (!ipv4 && !ipv6) problems.push("Enter an IPv4 or an IPv6 address.")
  if (ipv4 && !IPV4.test(ipv4)) problems.push("The IPv4 address is not valid.")
  if (ipv6 && (!ipv6.includes(":") || /[^0-9a-f:.]/i.test(ipv6))) {
    problems.push("The IPv6 address is not valid.")
  }
  const labels = hostname.split(".")
  if (
    labels.length < 2 ||
    !labels.every((label) => LABEL.test(label)) ||
    /^\d+$/.test(labels[labels.length - 1] ?? "")
  ) {
    problems.push("The host name must be fully qualified, such as out2.mail.example.com.")
  }
  return problems
}
