# Deliverability and IP pools

Deliverability is the question of whether the mail you send reaches the
inbox. Most of the answer lives at the receiving end, in the reputation
that mailbox providers attach to your sending IPs and domains. Camelmailer
gives you one direct lever over that reputation: **IP pools**, a way to
choose which source address a message leaves from, so you can keep
critical transactional mail on IPs that broadcast campaigns never touch.
The rest of this page covers what pools are, the exact order the worker
uses to pick a source address, how to manage pools, and the surrounding
practices (authentication, suppression hygiene, opt-in, warmup) that
decide whether those IPs earn a good reputation.

## IP pools

An **IP pool** is a named set of sending IP addresses. Each address in a
pool carries:

| Field | Meaning |
|---|---|
| `ipv4` | The IPv4 source address. Required and validated as a real IPv4. |
| `ipv6` | An optional IPv6 address. Bound for IPv6 connections when the pool sets an [address family](#address-family); stored for reference otherwise. |
| `hostname` | The EHLO / reverse-DNS name you intend the address to present. Must be a fully qualified host name; used as the EHLO name with `smtp_client.helo_from_ip_address`. |
| `priority` | Selection order within the pool. A lower number wins; the default is `100`. |

Pools and their addresses are installation-wide configuration (the
`ip_pools` and `ip_addresses` tables). A pool can be flagged `default`,
which marks it in the admin UI as your intended fallback pool. Deleting a
pool cascades to its addresses, and any server or stream that pointed at
it detaches cleanly and falls back (the foreign keys use
`ON DELETE SET NULL`).

Two things can be assigned a pool:

- A **server** has an optional default pool (`servers.ip_pool_id`). Every
  message the server sends uses this pool unless the message's stream
  overrides it.
- A **message stream** has an optional pool of its own
  (`message_streams.ip_pool_id`). When set, it overrides the server pool
  for that stream's mail. See [Message streams](streams.md).

## How the worker picks a source address

At delivery time the worker asks the store for one source address via
`source_ip_for(server_id, stream_id)`
(`crates/camelmailer-worker/src/worker.rs`). The resolution order is:

| Order | Source | When it applies |
|---|---|---|
| 1 | The stream's IP pool | The message has a stream and that stream sets `ip_pool_id`. |
| 2 | The server's IP pool | Otherwise, and the server sets `ip_pool_id`. |
| 3 | The host default address | Neither pool resolves to an address. |

The Postgres store expresses this as a single `COALESCE` of the stream
pool over the server pool, so a message with no stream, an unknown
stream, or a stream that leaves its pool blank all fall through to the
server pool. A transactional stream that never sets a pool therefore
resolves to exactly the server-pool address the worker used before
per-stream pools existed.

Within the chosen pool the worker takes the **highest-priority address**:
lowest `priority` number first, ties broken by insertion order (`id`).
Only the `ipv4` value is used (unless the pool sets an [address family](#address-family)), and the worker binds its outgoing SMTP
socket to that address, so the connection reaches the recipient from that
IP. When no pool resolves to an address, `source_ip` is `None` and the
worker sends from the host's default outbound address (the operating
system picks the source IP as it would for any outbound connection).

### Several worker hosts sharing a pool

With `worker.host_aware_pools: true` each worker only uses the pool
addresses its own host holds, so workers on several hosts can share one pool
and all send at the same time, each from its own IP.

```yaml
worker:
  host_aware_pools: true
  source_addresses: []            # empty: the addresses on this host's interfaces
  foreign_pool_delay_seconds: 30
```

- **Which addresses are local.** Without a list the worker reads the
  addresses configured on the host's network interfaces and rereads them
  every 30 seconds, so an address that moves to this host (a floating IP
  taken over by keepalived after a failure) is picked up without a restart.
  It does not probe by binding, which `net.ipv4.ip_nonlocal_bind=1` or
  `IP_FREEBIND` would make succeed for every address. The worker needs to see
  the host's interfaces (host networking in Docker). List the addresses in
  `source_addresses` (or `WORKER_SOURCE_ADDRESSES`) on a host where an
  address is configured on the interface but routed somewhere else.
- **Which one is used.** Among the local addresses, the lowest `priority`
  number wins. Ties go by a stable hash of the recipient domain, so one
  receiving domain keeps seeing the same IP from a host, which helps
  reputation and greylisting.
- **A pool with no address here.** Outgoing mail records its pool when it is
  queued, and the worker does not dequeue mail of pools its host holds no
  address of. Mail queued before that is handed back without counting an
  attempt, with its pool recorded, and looked at again after
  `foreign_pool_delay_seconds` (env `WORKER_FOREIGN_POOL_DELAY_SECONDS`, at
  least 1). The worker logs each such pool once, and again if the host held
  one of its addresses in between.
- **A pool that changed.** When a pool is deleted, or a server or stream moves
  to another pool, the pool recorded for its queued mail is stale. Every
  worker takes such mail, resolves the current pool, and sends or hands it
  back with the current pool recorded. One case stays on the old pool: the
  server moved but one of its streams still uses the old pool; that mail goes
  to a host holding the old pool, which hands it on.
- **The pool cannot be read** (a database error). The message goes back
  without an attempt and is retried after `foreign_pool_delay_seconds`
  rather than staying locked.
- **No pool at all.** The host's default address, as before. On several hosts
  that means several different source addresses, each of which needs SPF and
  reverse DNS; put every server on a pool before running more than one host.

Without the option every worker uses the pool's first address, exactly as
described above. Mail whose pool no running host holds an address of waits
in the queue without counting attempts, so watch the queue age.

## Why isolation matters

Mailbox providers score reputation per sending IP. A marketing blast that
draws spam complaints or hits spam traps drags down the reputation of
whatever IP sent it. If password resets, receipts, and one-time codes
share that same IP, they inherit the damage and start landing in spam at
the exact moment they matter most.

Separating the two is the point of per-stream pools. Put transactional
mail on the server's default pool and give your broadcast stream its own
pool, and the two reputations rise and fall independently. This pairs
directly with the stream model: transactional streams stay on trusted
IPs, while [Broadcast streams](broadcast.md) send from a pool you can
afford to expose to the volatility of marketing volume. Assigning a
distinct pool to a broadcast stream is the single most effective isolation
step Camelmailer offers.

## Address family

Which of a receiving host's addresses the worker connects to depends on the
address family, set installation-wide in `smtp_client.address_family` (see
[Configuration](configuration.md#address-family)) and per pool in the
pool's `address_family`:

| The message's pool | Remote addresses tried | Local address |
|---|---|---|
| None | `smtp_client.address_family` (default `dual_stack`: IPv6 first, alternating with IPv4) | The operating system's choice |
| A pool without `address_family` (the default) | IPv4 only | The address's `ipv4` |
| A pool with `address_family` | The pool's `address_family` | `ipv4` for IPv4 remotes, `ipv6` for IPv6 remotes |

A pool address without an `ipv6` value never sends over IPv6, whatever the
family: the bound address pins the family. A pool set to `ipv6_only` whose
address has no `ipv6` value cannot send, and each attempt fails softly
saying so. In every case the worker moves on to the next remote address
when a connection fails; `dual_stack` also races the two families
(`smtp_client.connection_attempt_delay_ms`, see
[Configuration](configuration.md#address-family)).

Two cases keep the order from before address families (every IPv4 address
first, from the operating system's address): a message whose pool has no
usable address (an empty pool, an `ipv4` value that does not parse), and
delivery through `smtp_relays`, where the smarthost and not the receiver
sees the connection. A pool still binds its addresses towards a relay.

Set the family to match the reverse DNS you can publish. A pool whose
addresses have PTR records for both families can use `dual_stack`; a pool
on a provider without IPv6 reverse DNS stays on the default, or uses
`prefer_ipv4` to keep IPv6 as a fallback for IPv6-only receivers.

```
PATCH /api/v2/admin/ip_pools/{id}
{ "address_family": "dual_stack" }
```

`"address_family": null` returns the pool to the default.

## Reverse DNS check

Each worker checks every address it may send from, at start and every
`smtp_client.reverse_dns_check_interval` seconds (an hour by default): both
values of every pool address, and the IPv4 and IPv6 address its host sends
from for servers without a pool. An address passes when it has a PTR record
and one of the PTR names resolves back to it (forward-confirmed reverse
DNS). Failures are logged as warnings, shown under **Admin → IP pools →
Reverse DNS of the source addresses**, and listed by
`GET /api/v2/admin/source_addresses`. With
`smtp_client.skip_family_without_reverse_dns: true`, a `dual_stack` or
`prefer_ipv4` connection leaves out the family whose source address failed
while the other's did not.

## Managing IP pools (admin)

Pools live under **Admin → IP pools** in the dashboard, and under the
`/api/v2/admin/ip_pools` management API (auth: `X-Admin-API-Key` or an
RBAC-scoped user session).

### Create a pool and add addresses

```
POST /api/v2/admin/ip_pools
{ "name": "Broadcast", "default": false }

POST /api/v2/admin/ip_pools/{pool_id}/ip_addresses
{ "ipv4": "203.0.113.20", "hostname": "bcast1.example.com", "priority": 100 }
```

`ipv4` and `hostname` are required; `ipv6` and `priority` are optional
(`priority` defaults to `100`). Add several addresses to one pool and the
lowest `priority` number is preferred, so you can order primary and
backup IPs.

The full CRUD surface:

| Method + path | Purpose |
|---|---|
| `GET/POST /api/v2/admin/ip_pools` | List and create pools |
| `GET/PATCH/DELETE /api/v2/admin/ip_pools/{id}` | Show, update (`name`, `default`, `address_family`) and delete a pool |
| `GET/POST /api/v2/admin/ip_pools/{pool_id}/ip_addresses` | List and add addresses |
| `GET/DELETE /api/v2/admin/ip_pools/{pool_id}/ip_addresses/{id}` | Show and remove an address |
| `GET /api/v2/admin/source_addresses` | The latest reverse-DNS check of every source address |

### Assign a pool to a server

The server's default pool is set from the server's settings, or via:

```
POST /api/v2/admin/organizations/{org}/servers/{server}/ip_pool
{ "ip_pool_id": 42 }
```

Send `"ip_pool_id": null` to clear it, which returns the server to the
host default address.

### Assign a pool to a stream

A stream's pool is set in the dashboard from the stream's **Edit** dialog
(the "IP pool" selector, where "Server default pool" means the stream sets
no pool of its own and inherits the server's). The current pool is shown
in the stream header next to its type and permalink. Over the API it is a
field of the stream update on the per-server surface:

```
PATCH /api/v2/server/streams/{permalink}
{ "ip_pool_id": 42 }
```

The pool id refers to one of the installation-wide pools above. Send
`"ip_pool_id": null` to detach the stream and fall back to the server
pool.

## Practical deliverability guidance

Pools decide which IP mail leaves from. Whether that IP is trusted depends
on the practices below. The first two are backed by product features; the
rest are operational.

### Authenticate every sending domain

SPF, DKIM, and DMARC are what let a receiver tie your mail to your domain
and trust it. Camelmailer signs outbound mail with DKIM at delivery time
(the domain's own key when it has one, the installation key otherwise) and
can verify your published records. Set each domain up and keep it green:
see [Sending domains](domains.md) for SPF/DKIM/DMARC publication and
per-domain keys, and [DMARC monitoring](dmarc.md) for the health check,
aggregate-report ingestion, and the `p=none → quarantine → reject` policy
journey. A domain that fails alignment will struggle regardless of which
IP it sends from.

### Keep the recipient list clean

Sending to addresses that bounce or complain is the fastest way to lose
reputation. Camelmailer holds any message to a suppressed recipient before
it ever reaches the wire, and it grows the suppression list automatically
from hard bounces and complaints. Review it and keep bad addresses out of
your sends: see [Suppressions](suppressions.md).

### Require opt-in and honor unsubscribes for broadcast

Broadcast mail carries obligations that transactional mail does not.
Camelmailer gates broadcast sends on a per-stream opt-in and wires up
one-click unsubscribe: an unsubscribe or a spam complaint creates a
stream-scoped suppression and flips the recipient's subscription closed,
so the next campaign skips them. Lean on this rather than sending to
anyone who has not agreed to hear from you. See
[Broadcast streams](broadcast.md).

### Warm up new IPs

A brand-new sending IP has no reputation, and providers throttle unknown
senders that suddenly push high volume. Warm a new pool by starting with a
low daily volume of your most engaged recipients and increasing it
gradually over days to weeks, watching your DMARC pass rate and bounce and
complaint figures as you go. Camelmailer does not pace this for you; it is
a schedule you run by controlling how much you send through the new pool.

### Publish matching reverse DNS

Receivers expect the sending IP to have a PTR record that resolves back,
and they check the EHLO name your server presents. Set the reverse DNS for
each pool address at your network provider, and set the installation's
EHLO identity to a name that matches (`camelmailer.smtp_hostname`, or an
explicit `smtp_server` HELO override; see [Configuration](configuration.md)).
With several pool addresses, each with its own PTR, turn on
`smtp_client.helo_from_ip_address`: the worker then greets with the
`hostname` of the address a delivery is bound to, so EHLO, PTR and A record
agree per IP. Without it the `hostname` field only records the name you
intend an IP to present. The API accepts only a fully qualified host name
there (no IP literal, no trailing dot); a stored name that is not one is
skipped with a warning and the worker greets with `dns.helo_hostname`.

## What Camelmailer enforces versus operational best practice

Being honest about the line between the two matters, because a green
dashboard does not by itself guarantee inbox placement.

| Concern | Status |
|---|---|
| Source IP selection (stream pool → server pool → host default) | Enforced by the worker |
| Binding the outgoing connection to the chosen pool address | Enforced |
| DKIM signing of authenticated domains at delivery | Enforced |
| Suppression gate before send; auto-suppress on bounce/complaint | Enforced |
| Broadcast opt-in gate and one-click unsubscribe handling | Enforced |
| SPF / DMARC record publication and alignment | Your DNS; Camelmailer checks and monitors |
| Reverse DNS (PTR) for each pool IP | Your network provider; checked by the workers (forward-confirmed), reported, and avoided only with `skip_family_without_reverse_dns` |
| EHLO / HELO hostname | The installation-level `dns.helo_hostname` (else `smtp_hostname`); with `smtp_client.helo_from_ip_address: true`, the `hostname` of the pool address the delivery is bound to |
| IPv6 source sending | Bound for IPv6 connections when the pool sets an `address_family`; otherwise only `ipv4` is bound |
| Pool `default` flag | A label in the admin UI; source resolution reads a server's or stream's assigned pool, so assign a pool explicitly for it to take effect |
| IP warmup pacing | Operational; you control volume ramp, the product does not throttle |

Read that last column as the work that stays with you. Camelmailer places
mail on the IPs and with the signatures you configure, and it keeps your
lists clean; earning and holding the reputation on those IPs is the
ongoing operational job that pools are built to protect.
