# Infrastructure dashboard

The **Infrastructure** section of the dashboard (global administrators only,
under *Admin*) shows how the installation is built and whether the DNS and the
sending addresses are in order. Every page is read-only apart from the warm-up
mode; none of them changes DNS, pools or nodes.

| Page | What it shows | API |
|---|---|---|
| Topology | Nodes, roles, versions, heartbeats, queue leases, load balancer, IP pools | `GET /api/v2/admin/cluster` |
| DNS checks | Every record the setup depends on, found versus expected, with the record to set | `GET /api/v2/admin/dns_checks` |
| Add server / add IP | The records a new address needs, re-checked until green | `GET /api/v2/admin/dns_checks/candidate` |
| IP metrics & warm-up | Counters per address and provider; warm-up mode, allowance and decisions | `GET /api/v2/admin/ip_metrics`, `GET /api/v2/admin/ip_warmup` |
| Provider feedback | Postmaster Tools, SNDS, feedback loops and seed placement, with freshness | `GET /api/v2/admin/deliverability/feedback` |

## Topology

One row per running process: node name, role (`web`, `smtp`, `worker`),
version, the time since its last heartbeat and its status:

- **live**: heartbeat within `cluster.node_stale_seconds` (90);
- **stale**: no heartbeat for longer; the process probably died, or cannot
  reach the database;
- **stopped**: the process shut down cleanly.

For workers the page lists the queue leases they hold (delivery queue,
webhooks, lifecycle events) and the age of the oldest lock. Locks held under
an id no registered worker has belong to a worker that is gone; they move on
once the lease runs out. Above the table: the load balancer address and the
inbound hostname from the `cluster` configuration, and a warning when live
processes run different versions (expected only during a rolling update).
Below it: every IP pool with its addresses, their IPv4/IPv6 values and host
names.

## DNS checks

The checks are grouped per sending address and per host name. Each shows a
status (ok, warn, fail), what DNS returned, what is expected, and the record
to set as one line you can copy, for example:

```
set A out1.mail.example.com → 203.0.113.21, TTL 300
set PTR 203.0.113.21 → out1.mail.example.com (at your hosting provider)
```

*Copy all failing instructions* puts every line that is not ok on the
clipboard, ready for a ticket to the DNS or hosting team. What is checked:

- A/AAAA of the inbound hostname point at the load balancer;
- the PTR record of every sending address (both values of every pool address,
  and the public addresses of the nodes for servers without a pool);
- forward-confirmed reverse DNS: the PTR name resolves back to the address;
- the EHLO name the worker uses from the address equals its PTR name;
- SPF of the return-path domain and of every verified sending domain covers
  every sending address, with the exact `ip4:`/`ip6:` mechanisms to add;
- every verified sending domain publishes the DKIM key the worker signs with
  (the exact `v=DKIM1; k=rsa; p=…` record when it does not) and one DMARC
  record (`p=none` is a warning, with the record to move to `p=quarantine`);
- domains with an inbound route and the return-path domain have their MX on
  the installation (a wrong MX fails); the tracking host name resolves.

Results are cached for a minute; *Refresh* looks everything up again.

## Add server / add IP

The wizard walks through a new sending address before it carries any mail:
enter its IPv4 and IPv6 values and the planned host name (`out2.mail.example.com`),
and it lists the records to create: the A and AAAA records, the PTR at your
hosting provider, the SPF mechanisms, and the host name to store on the pool
address for the EHLO greeting. While the wizard is open it re-checks every
30 seconds (or on demand) and ticks each record off as it appears. When all
are green it links to the IP pools page, where you add the address to a pool.
It only reads DNS.

## IP metrics & warm-up

**Metrics**: per sending address and per pool, over 1 to 90 days: accepted,
deferred and bounced attempts, policy refusals and complaints, the deferral,
bounce and policy-block rates, and the complaint rate; broken down by
receiving provider group (built in: Microsoft, Google, Yahoo, Apple, United
Internet, T-Online, others; more via `provider_groups`) and by day.

**Warm-up** is off by default. Per address the page shows the mode and where
it comes from (the address itself, its pool, or the default), and per provider
the daily allowance (each provider group has its own), today's use, the
status (not started, ramping, holding,
slowed, paused, warm), the last decision and the last brake with their
reasons, the progress towards the cap and an estimate of the healthy days
until warm. The mode can be changed per address or per pool at any time
(off, slow, normal, fast, custom); the ramp continues from the current
allowance. The decision log lists every step. How the modes and the brake
work: [Deliverability](deliverability.md#adaptive-ip-warm-up).

## Provider feedback

What the mailbox providers report about the installation's domains and
addresses, each source with the time of its last fetch and its last error:
Google Postmaster Tools (reputation, spam rate), Microsoft SNDS (filter
result, complaint rate, trap hits), the complaint feedback loops (Microsoft
JMRP, Yahoo CFL), and inbox placement tests with your own seed mailboxes,
whose every result is kept with its time (filterable history per address and
provider group; the warm-up brake uses the worst result of the last 24 hours).
Each source is optional and only appears once it is configured. Setup:
[Provider feedback](provider-feedback.md).

## See also

- [High availability](high-availability.md): architecture, node roles,
  configuration and failure modes
- [Deliverability and IP pools](deliverability.md)
