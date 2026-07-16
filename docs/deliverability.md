# Deliverability and IP pools

Deliverability is the question of whether the mail you send reaches the
inbox. Most of the answer lives at the receiving end, in the reputation
that mailbox providers attach to your sending IPs and domains. CamelMailer
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
| `ipv6` | An optional IPv6 address. Stored for reference. |
| `hostname` | The EHLO / reverse-DNS name you intend the address to present. |
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
Only the `ipv4` value is used, and the worker binds its outgoing SMTP
socket to that address, so the connection reaches the recipient from that
IP. When no pool resolves to an address, `source_ip` is `None` and the
worker sends from the host's default outbound address (the operating
system picks the source IP as it would for any outbound connection).

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
step CamelMailer offers.

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
| `GET/DELETE /api/v2/admin/ip_pools/{id}` | Show and delete a pool |
| `GET/POST /api/v2/admin/ip_pools/{pool_id}/ip_addresses` | List and add addresses |
| `GET/DELETE /api/v2/admin/ip_pools/{pool_id}/ip_addresses/{id}` | Show and remove an address |

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
and trust it. CamelMailer signs outbound mail with DKIM at delivery time
(the domain's own key when it has one, the installation key otherwise) and
can verify your published records. Set each domain up and keep it green:
see [Sending domains](domains.md) for SPF/DKIM/DMARC publication and
per-domain keys, and [DMARC monitoring](dmarc.md) for the health check,
aggregate-report ingestion, and the `p=none → quarantine → reject` policy
journey. A domain that fails alignment will struggle regardless of which
IP it sends from.

### Keep the recipient list clean

Sending to addresses that bounce or complain is the fastest way to lose
reputation. CamelMailer holds any message to a suppressed recipient before
it ever reaches the wire, and it grows the suppression list automatically
from hard bounces and complaints. Review it and keep bad addresses out of
your sends: see [Suppressions](suppressions.md).

### Require opt-in and honor unsubscribes for broadcast

Broadcast mail carries obligations that transactional mail does not.
CamelMailer gates broadcast sends on a per-stream opt-in and wires up
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
complaint figures as you go. CamelMailer does not pace this for you; it is
a schedule you run by controlling how much you send through the new pool.

### Publish matching reverse DNS

Receivers expect the sending IP to have a PTR record that resolves back,
and they check the EHLO name your server presents. Set the reverse DNS for
each pool address at your network provider, and set the installation's
EHLO identity to a name that matches (`camelmailer.smtp_hostname`, or an
explicit `smtp_server` HELO override; see [Configuration](configuration.md)).
The `hostname` field on a pool address records the name you intend an IP to
present, which keeps your operational intent next to the address.

## What CamelMailer enforces versus operational best practice

Being honest about the line between the two matters, because a green
dashboard does not by itself guarantee inbox placement.

| Concern | Status |
|---|---|
| Source IP selection (stream pool → server pool → host default) | Enforced by the worker |
| Binding the outgoing connection to the chosen pool address | Enforced |
| DKIM signing of authenticated domains at delivery | Enforced |
| Suppression gate before send; auto-suppress on bounce/complaint | Enforced |
| Broadcast opt-in gate and one-click unsubscribe handling | Enforced |
| SPF / DMARC record publication and alignment | Your DNS; CamelMailer checks and monitors |
| Reverse DNS (PTR) for each pool IP | Your network provider; not verified by the product |
| EHLO / HELO hostname | A single installation-level config value, not the per-address `hostname` field |
| IPv6 source sending | The `ipv6` field is stored but source binding uses the `ipv4` value |
| Pool `default` flag | A label in the admin UI; source resolution reads a server's or stream's assigned pool, so assign a pool explicitly for it to take effect |
| IP warmup pacing | Operational; you control volume ramp, the product does not throttle |

Read that last column as the work that stays with you. CamelMailer places
mail on the IPs and with the signatures you configure, and it keeps your
lists clean; earning and holding the reputation on those IPs is the
ongoing operational job that pools are built to protect.
