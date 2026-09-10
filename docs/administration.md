# Administration and abuse monitoring

An installation of Camelmailer carries mail for every organization on it,
and its sending reputation is shared. One tenant blasting a purchased list
costs every other tenant inbox placement, so an operator needs to see what
is moving through the service and act on it early. The administration area
in the dashboard is that view, and this page covers what it shows, how to
read it, and the two interventions available when something is wrong.

Everything here requires a **global administrator** account (the `admin`
flag on the user, set with `make-user --admin` or from Administration →
Users) or an unscoped admin API key. Organization roles reach their own
organization only.

## The administration area

Open the **Admin** pill in the top bar. The sidebar then lists the
installation-wide pages:

| Page | What it covers |
|---|---|
| Organizations | Every tenant with its traffic, the flags worth a look, and the drill-down per organization |
| Users | Every account on the installation, the admin flag, and account deletion |
| IP pools | Sending addresses and their pools, see [Deliverability](deliverability.md) |
| Admin API keys | Machine credentials for `/api/v2/admin`, listable and revocable |
| Audit log | Authentication events across the installation |

## The organizations view

The Organizations page opens with a strip of installation numbers
(organizations, servers, users, outgoing volume, bounce rate, held mail),
then lists every tenant. The window switch above the table (24h, 7d, 30d)
changes which window the volume, bounce and held columns report; the strip
follows it.

Each row carries:

| Column | Meaning |
|---|---|
| Organization | Name and permalink. Opens the organization's detail page. |
| Status | `Sending` (mail in the last 24 hours), `Quiet` (mail in the last 30 days, none today), `Idle` (nothing in 30 days), `Suspended` (every server suspended). |
| Flags | Zero or more signals worth attention, described below. |
| Servers | How many mail servers the tenant has, with a note when some are suspended. |
| Outgoing | Messages sent in the selected window. |
| Bounces | Bounced share of the window's outgoing messages, red above 10%. A bounce is an outgoing-mail outcome, so inbound mail stays out of the denominator. |
| Held | Messages held in the selected window. |
| Last message | When the tenant last sent or received anything. |
| 2FA | Whether the organization enforces two-factor authentication. |

Sorting a column reorders the table, and the search box matches names and
permalinks. The default order is busiest first over 30 days, so the tenant
worth looking at leads the list.

### The flags

Flags are judged over the last 30 days and compose, so a newly active
tenant with a bounce problem carries both. When any tenant is flagged, a strip
above the table names them with the reason.

| Flag | When it appears | Why it matters |
|---|---|---|
| **Newly active** | The oldest message inside the 30-day window arrived in the last 24 hours, and the tenant is sending now. | Covers both shapes worth a look: a fresh signup sending immediately at volume, and an account dormant for a month that suddenly starts. The counters cannot tell those apart, because the window bounds what they can see, so the label claims only what is known. |
| **Check bounces** | More than 10% of at least 20 outgoing messages bounced. | A bounce share that high usually means a list the sender did not collect themselves. The floor of 20 keeps a two-message sample from raising it. |
| **Held mail** | Any message was held in the window. | Outbound spam scoring holds mail above the server's threshold, so held mail is the spam filter's own verdict. |
| **Failing** | Any message ended in `HardFail` or `SoftFail`. | Repeated failures point at a misconfigured domain or a receiving side that is refusing this sender. |

The flags describe counters, and they decide nothing on their own. A
transactional tenant with a genuinely stale list and a marketing tenant
buying addresses look similar from here, so the drill-down and the message
logs are where a judgment gets made.

## One organization in detail

Clicking a row opens the organization's page. It shows:

- **Traffic**: the three windows as rows, with outgoing, inbound,
  delivered, held, failed, bounced and the bounce rate as columns. The
  line above it dates the first and last message inside the widest window.
- **Servers**: one row per mail server with its mode, state, outgoing
  volume per window, 30-day bounce rate and held count, its send limit,
  and when it last carried a message. Each row links into the server so
  the message log, domains and credentials are one click away.
- **People**: who has access, with their role and how long they have had
  it.
- **Danger zone**: deleting the organization.

"Open organization" in the header enters the tenant's own dashboard, where
the message logs, streams and suppressions live. A global administrator
reaches every organization there, including ones they are not a member of.

## Suspending a server

Suspension is the reversible intervention, and it is the right first move
while something is still being investigated. Each server row has a
**Suspend** button; the server dashboard has the same control under
Suspension & deletion, with an optional reason.

A suspended server stops accepting new mail. Its data, credentials and
message history stay intact, and **Unsuspend** puts it straight back into
service. Suspending one server leaves the tenant's other servers running,
so a single bad stream can be stopped without taking a customer offline.

## Deleting an organization

Deletion is permanent and it cascades: every server of the organization
goes with it, and with each server its domains, credentials, routes,
webhooks, suppressions and stored messages, plus the organization's
memberships and invitations. Nothing survives that a later report could be
checked against, so collect what you need from the message logs first.

The confirmation asks for the organization's permalink to be typed out.
That is deliberate: the deletion is triggered from a list of tenants,
where a mistaken row is easy to hit and impossible to undo.

## The API behind the view

Both pages read the same two endpoints, so an operator can script the same
oversight or feed it into monitoring.

```bash
# Every organization on the installation, busiest first.
curl -s https://mail.example.com/api/v2/admin/overview \
  -H "X-Admin-API-Key: $ADMIN_API_KEY" | jq '.data.overview.organizations[0]'
```

```json
{
  "name": "Acme",
  "permalink": "acme",
  "require_two_factor": false,
  "servers": 2,
  "suspended_servers": 0,
  "members": 3,
  "day":   { "total": 412, "outgoing": 410, "incoming": 2, "sent": 402, "held": 1, "failed": 3, "bounced": 6 },
  "week":  { "total": 2911, "…": 0 },
  "month": { "total": 11840, "…": 0 },
  "first_message_at": "2026-08-13T07:41:02Z",
  "last_message_at": "2026-09-11T09:12:44Z"
}
```

The response also carries `instance` with the same counters summed across
every tenant plus the organization, server, user and suspended-server
counts, and `windows` with the exact instant each window starts at.

```bash
# One organization, broken down by server.
curl -s https://mail.example.com/api/v2/admin/organizations/acme/overview \
  -H "X-Admin-API-Key: $ADMIN_API_KEY" | jq '.data.overview.servers'
```

Each server row adds `mode`, `suspended`, `suspension_reason` and
`send_limit` to the same window counters.

`GET /api/v2/admin/overview` is reserved for administrators of the
installation: a session without the admin flag answers `403 Forbidden`,
and an organization- or server-scoped admin key answers `404`. The
per-organization endpoint is a read any member of that organization may
perform, and a non-member answers `404` so the existence of other tenants
stays private.

Both endpoints report counters over three fixed windows (24 hours, 7 days,
30 days) and answer `503 StorageUnavailable` on an installation running
without message storage. Zeros would read as real numbers.

## Send limits

A per-server send limit caps outgoing messages in a trailing 30-day
window, and it is writable by global administrators only (see
[Sending email](sending.md)). It works as a guardrail on a tenant you have
reason to watch: the server dashboard's Send limit field sets it, the
Servers table on the organization page shows it, and a server over its
limit refuses further sends with an error the sender can see.

## What this view does not do

- It reports counters, and it raises no alert of its own. Nothing here
  emails or pages anybody.
- It reads message counts, and it never opens message content. Reading a
  tenant's mail happens in that tenant's own message log, with everything
  the audit log records about it.
- It knows only what the message rows say. A tenant sending through an
  SMTP relay that silently drops mail looks healthy here, and the
  receiving side is the only place that knows better.
