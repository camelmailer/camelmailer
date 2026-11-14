# High availability

This page describes how to run Camelmailer on several hosts so that the loss
of one host, a restart or a rolling update does not stop mail from coming in
or going out. It covers the architecture, which processes may run more than
once, the configuration each node needs, the DNS records the setup depends
on, what happens when something fails, and how to roll back.

Everything here works with one PostgreSQL database. Making PostgreSQL itself
highly available (Patroni, a managed database, a replica with failover) is
outside Camelmailer and works the same as for any other application: point
`postgres.host` at the address that always reaches the current primary.

## Architecture

```
                         MX / submission clients
                                   |
                     mail.example.com (A/AAAA)
                                   |
                       +-----------------------+
                       |  TCP load balancer    |   PROXY protocol v1
                       |  25, 465, 587         |   (send-proxy)
                       +-----------------------+
                         |                   |
                +-----------------+   +-----------------+
                | node 1          |   | node 2          |
                | smtp-server     |   | smtp-server     |
                | web-server      |   | web-server      |
                | worker          |   | (worker)        |
                | sends from      |   | sends from      |
                | out1.example.com|   | out2.example.com|
                +-----------------+   +-----------------+
                         \                   /
                          +-----------------+
                          |   PostgreSQL    |  queue, leases, node registry
                          +-----------------+
```

- **Inbound** mail and submission reach one public name (the *inbound
  hostname*, here `mail.example.com`), which points at the load balancer. The
  balancer spreads TCP connections over the `smtp-server` processes and puts a
  PROXY protocol v1 line in front of each, so the SMTP server sees the real
  client address for SPF and IP-based credentials.
- **Outbound** mail leaves from the IP addresses of the IP pools. Each address
  has its own identity: a host name (`out1.example.com`), an A record, a PTR
  record with the same name, and the EHLO greeting the worker uses from it.
- **The web server** (HTTP API and dashboard backend) is stateless and runs on
  as many nodes as you like behind an HTTP load balancer or reverse proxy.
- **Workers** on any number of nodes share one queue in PostgreSQL. A message
  is handed to exactly one worker at a time; leases and heartbeats make sure a
  lost worker's mail moves on to another one.

## Which processes may run more than once

| Process | More than once? | Notes |
|---|---|---|
| `web-server` | Yes | Stateless. Sessions and everything else live in PostgreSQL. |
| `smtp-server` | Yes | Behind a TCP load balancer with `smtp_server.proxy_protocol: true`. Each process accepts and queues independently. |
| `worker` | Yes | Shares the queue through row locks (`SKIP LOCKED`), leases and heartbeats. Housekeeping, retention, the reverse-DNS check and the nightly warm-up evaluation are safe to run on every worker: each step is idempotent or decided by the first worker to save it. |
| `initialize` | Once per deploy | Runs the database migrations. Run it once, before the new version starts anywhere; do not run two at the same time. |
| PostgreSQL | One primary | Use your database's own HA. Camelmailer needs one writable primary. |
| In-memory mode (`postgres.enabled: false`) | No | For development only; every process would have its own data. |

A node does not have to run every role. A common split is two nodes with
`smtp-server` and `web-server` each and a worker on the node that holds the
sending IP addresses.

## Inbound: load balancer and PROXY protocol

Configure the balancer for plain TCP on 25, 465 and 587 and enable PROXY
protocol **v1** towards the backends (HAProxy `send-proxy`, Hetzner load
balancer "proxy protocol", AWS NLB proxy protocol v1). On every
`smtp-server` behind it:

```yaml
smtp_server:
  proxy_protocol: true
```

- On port 25 and 587 (`smtp` listeners) the PROXY line precedes the banner.
  On 465 (`smtps`) the balancer sends it in the clear in front of the client's
  TLS handshake; the server reads it first, then starts TLS.
- `PROXY UNKNOWN`, which balancers use for their own health checks, is
  accepted.
- A connection without a valid header is closed. Do not expose a
  `proxy_protocol` listener to clients that bypass the balancer. To serve
  clients directly as well, run a second `smtp-server` without it.
- Refused headers are logged as a warning at most once a minute: a balancer
  without `send-proxy`, or one sending v2, shows up there.

Health checks: a TCP check on port 25 is enough. The balancer should take a
node out when the check fails, and put it back when it passes again.

See [Configuration](configuration.md#behind-a-tcp-load-balancer-proxy_protocol)
for the details of the header handling.

## Outbound: workers, leases, heartbeats and shutdown

### Leases and heartbeats

A worker locks each queue row it works on (`locked_by` is its worker id) and
refreshes the lock every 30 seconds while the delivery runs. A row whose lock
has not been refreshed for the **lease** is treated as abandoned and taken by
the next worker.

```yaml
camelmailer:
  queued_message_lock_stale_seconds: 300
```

Set the lease to about **300 seconds** on every node. The default (one day,
`queued_message_lock_stale_days`) exists only so an upgrade changes nothing;
with it, mail held by a worker that died waits a day. Values below 90 are
raised to 90. The same lease applies to the webhook queue and the lifecycle
outbox.

A worker records the result of a delivery only while it still holds the row.
If a worker stalls for a whole lease (no heartbeat reached the database) and
another worker takes the row over, the first worker's late result is not
recorded; the row belongs to the new owner and the stalled worker logs a
warning.

### Graceful shutdown

On SIGTERM or SIGINT a worker stops taking new work and gives the deliveries
in flight `worker.shutdown_grace_seconds` (default 8) to finish. Then:

- a delivery that has not sent `DATA` yet (connecting, EHLO, STARTTLS,
  `MAIL FROM`, `RCPT TO`) goes straight back to the queue without counting an
  attempt, and another worker sends it;
- a delivery that has sent `DATA` keeps its lock for the lease, because the
  receiver may already have accepted it. With a lease of 300 seconds it moves
  again within five minutes.

Keep the process manager's stop timeout above the grace period (Docker:
`stop_grace_period`, systemd: `TimeoutStopSec`). The compose file ships
`init: true` and `stop_grace_period: 30s` for the worker.

### Where each worker sends from

With one worker host, the pool's addresses are simply bound on that host. With
workers on several hosts sharing a pool, turn on host-aware pools so that each
worker only uses the addresses its own host holds:

```yaml
worker:
  host_aware_pools: true
  source_addresses: []   # empty: the addresses on this host's interfaces
```

Mail for a pool with no address on a host is left for the hosts that have
one. List `source_addresses` explicitly where an address is on the interface
but routed elsewhere (floating IPs that are not active on this host). See
[Deliverability](deliverability.md#several-worker-hosts-sharing-a-pool).

### Address family

```yaml
smtp_client:
  address_family: dual_stack   # dual_stack | prefer_ipv4 | ipv4_only | ipv6_only
```

`address_family` applies to servers **without** an IP pool. A pool can set
its own family; without one a pool sends from each address's IPv4 value only.
Only send over IPv6 from an address that has a PTR record and an SPF entry;
the reverse-DNS check (`GET /api/v2/admin/source_addresses`) shows which
addresses pass.

## Node registry and the topology view

Every process registers itself in the database with its node name, role,
version and public addresses, and refreshes that entry every
`cluster.heartbeat_interval_seconds`. A worker registers under its lock id, so
the topology view can show which node holds which queue leases.

```yaml
cluster:
  node_name: node-1                    # default: the host name (CAMELMAILER_NODE_NAME)
  load_balancer_address: 203.0.113.10  # the balancer's public address
  inbound_hostname: mail.example.com   # the name MX records point at
  public_addresses: [203.0.113.21]     # this host's public addresses (CAMELMAILER_PUBLIC_ADDRESSES)
  heartbeat_interval_seconds: 30
  node_stale_seconds: 90
```

`public_addresses` matters when the process cannot see its public address,
for example in a Docker bridge network where it only sees `172.18.x`. The
reverse-DNS check then checks the listed address. `load_balancer_address` and
`inbound_hostname` are used by the DNS checks below. The infrastructure
dashboard and `GET /api/v2/admin/cluster` show every node as live, stale or
stopped, and warn when live nodes run different versions.

## The DNS model

| Record | Points at | Why |
|---|---|---|
| `A`/`AAAA mail.example.com` | the load balancer | The inbound hostname: MX targets and submission clients use it |
| `MX example.com` (and every tenant domain that receives mail here) | `mail.example.com` | Inbound routing |
| `A outN.example.com` | the sending address N | The outbound identity of each address |
| `PTR` of sending address N | `outN.example.com` | Set at the hosting provider. Receivers expect forward-confirmed reverse DNS: the PTR name resolves back to the same address |
| EHLO from address N | `outN.example.com` | Store `outN.example.com` as the pool address `hostname` and set `smtp_client.helo_from_ip_address: true`, so EHLO, PTR and A record agree per address |
| SPF of the return-path domain and of each sending domain | covers every sending address (`ip4:`/`ip6:` or an include that does) | Receivers check SPF against the connecting address |

The load balancer's address is for inbound only; it never sends. Each sending
address keeps its own name, so an address can move between hosts (a floating
IP) without any DNS change.

The **DNS checks** of the infrastructure dashboard (and
`GET /api/v2/admin/dns_checks`) look up every one of these records and show,
per address and per host name, what was found and the exact record to set.
The **add server / add IP wizard** lists the records a new address needs and
re-checks them until they are all in place. Both only read DNS; they never
change it.

## Rolling updates

1. Run `camelmailer initialize` once with the new version. Migrations only
   add tables and columns, so the old version keeps working against the new
   schema.
2. Update the nodes one at a time: take a node out of the load balancer (or
   let the health check do it), stop it, start the new version, wait until it
   is live in the topology view, put it back.
3. Workers hand back what they have not started and keep what may already be
   with a receiver (see graceful shutdown). With a lease of 300 seconds the
   kept rows move again within five minutes.
4. The topology view warns about mixed versions until every node runs the new
   one.

## Failure modes

| What fails | What happens | What to do |
|---|---|---|
| A worker process crashes | Its rows stay locked for the lease, then another worker takes them. A delivery that had sent `DATA` may be sent a second time if the receiver had accepted it; that is the same as with any MTA after a crash. | Keep the lease at about 300 seconds. |
| A whole node is lost | The balancer stops sending it connections. Its worker's rows move on after the lease. Mail for a pool address that only that host held waits until a host holds the address again (host-aware pools) or goes out from the remaining addresses of the pool. | Move the floating IP, or rely on the other pool addresses. |
| A worker stalls without crashing (no database for longer than the lease) | Another worker takes its rows. When the stalled worker comes back, its late results are not recorded and it logs a warning naming the rows. | Investigate the database connection of that host. |
| The load balancer sends no or the wrong PROXY header | Every connection to a `proxy_protocol` listener is closed; warnings in the SMTP server log. | Enable PROXY protocol v1 on the balancer, or turn `proxy_protocol` off. |
| PostgreSQL is unreachable | Intake answers with temporary failures, delivery pauses; nothing is lost that was already queued. | Database HA; Camelmailer reconnects by itself. |
| A sending address loses its PTR or SPF entry | Receivers defer or refuse mail from it. The reverse-DNS check and the DNS checks turn red; the warm-up brake (when enabled for that address) pauses it for the affected provider. | Fix the record; the checks turn green on their next run. |
| Mixed versions after an aborted update | Both versions work against the newer schema. | Finish or roll back the update. |

## Rollback

- **Configuration only** (`cluster`, lease, `host_aware_pools`, `proxy_protocol`):
  revert the setting and restart the process. Removing the `cluster` group only
  removes information from the dashboard.
- **To an earlier version**: stop the nodes one at a time and start the earlier
  version. Migrations are additive, so the earlier version runs against the
  newer schema; tables it does not know (node registry, counters, warm-up) are
  simply not used. Do not run the earlier version's `initialize` against a
  newer schema expecting it to remove anything; it removes nothing.
- **Warm-up**: set every address and pool to `off` (or `inherit` with the pool
  `off`). The worker stops consulting warm-up within a minute and sends as
  before.

## See also

- [Configuration](configuration.md): every key mentioned here
- [Deliverability and IP pools](deliverability.md): pools, host-aware pools,
  address family, reverse DNS, IP metrics and the adaptive warm-up
- [Infrastructure dashboard](infrastructure-dashboard.md): topology, DNS
  checks, the wizard, IP metrics and warm-up
