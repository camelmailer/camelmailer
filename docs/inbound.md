# Inbound mail and routing

CamelMailer receives mail as well as sends it. An **inbound stream**
groups the mail that arrives at a server, and a **route** decides what
happens to each incoming message: hand it to an HTTP endpoint, keep it
for later inspection, or ingest it internally (for example DMARC
aggregate reports). This page covers how mail reaches the server, the
route model and its target kinds, how the worker processes a routed
message, and how to manage routes over the admin API and in the
dashboard.

Inbound streams themselves are described in
[Message streams](streams.md); this page is about what a route does once
mail has arrived.

## How inbound mail arrives

The SMTP server (`smtp`, port 25 in the [Quickstart](quickstart.md)
stack) listens as the MX for the domains you host and accepts inbound
mail at `RCPT TO` time. It splits each recipient into a local part, an
optional `+tag`, and a domain, then matches it in this order:

1. **Return path** (`dns.return_path_domain`, or a custom
   `return_path_prefix.<domain>`): the address carries a server token and
   the message is booked as a bounce for that server.
2. **Direct-to-route** (`dns.route_domain`): the local part is a route
   **token**, so the address `<token>@<route_domain>` reaches exactly one
   route regardless of that route's own address. A `+tag` is preserved on
   the rebuilt recipient.
3. **Route address**: the local part and domain match a route's `name`
   and its domain (`<name>@<domain>`). This is the normal way a person or
   system addresses your inbound mail.

A matched recipient is accepted with `250 OK`, stored, and queued for the
worker with its `route_id` attached. Two conditions are answered right at
`RCPT TO`: a suspended mail server replies `535`, and a route in
**Reject** mode replies `550 Route does not accept incoming messages` so
the sender learns immediately.

The route domains come from the `dns` config group
(`return_path_domain`, `route_domain`); see
[configuration.md](configuration.md).

## The route model

A route is a small config record on one mail server:

| Field | Meaning |
|---|---|
| `name` | The local part the route answers to (`support`, `dmarc`, `replies`). |
| `domain_id` | The domain the address lives under. When empty, the route is reachable only by token at `<token>@<route_domain>`. |
| `token` | An opaque handle for the direct-to-route address; also useful as a stable identifier. |
| `mode` | What to do with matched mail (see below). |
| `endpoint_url` | The delivery target for **Endpoint** mode: an HTTP(S) URL or `internal://dmarc-reports`. |

`mode` is one of five values and decides the message's fate:

| Mode | At `RCPT TO` | At the worker |
|---|---|---|
| `Endpoint` | accepted | delivered to `endpoint_url` (see target kinds below) |
| `Accept` | accepted | stored; there is nothing further to deliver |
| `Hold` | accepted | stored and kept on the server for inspection |
| `Bounce` | accepted | stored; no separate bounce action runs today |
| `Reject` | refused with `550` | never reached (the sender was already told) |

`Accept`, `Hold`, and `Bounce` all result in the message being stored and
readable in message history with nothing sent onward. `Endpoint` is the
only mode that forwards the message somewhere, and where it goes depends
on `endpoint_url`.

## Target kinds

For a route in `Endpoint` mode, the `endpoint_url` names the target. Two
kinds exist, and route validation accepts exactly these shapes (any other
`internal://` value, or a non-URL string, is rejected with `422`
`ValidationError`):

| `endpoint_url` | Target kind | What the worker does |
|---|---|---|
| `https://…` or `http://…` | **HTTP endpoint** | POSTs a JSON envelope carrying the raw message (base64) to the URL. This is the webhook-style delivery of inbound mail; see [Webhooks](webhooks.md) for the signing and retry model shared across CamelMailer's HTTP deliveries. |
| `internal://dmarc-reports` | **Internal DMARC ingestion** | Parses the message as an RFC 7489 aggregate report and stores it in the tenant's report tables. Documented in full under [DMARC monitoring](dmarc.md). |

The HTTP POST body is:

```json
{
  "message": {
    "id": 1234,
    "token": "…",
    "rcpt_to": "support@acme.example",
    "mail_from": "customer@example.com",
    "bounce": false
  },
  "raw_base64": "…the full RFC 822 message, base64-encoded…"
}
```

Your endpoint owns everything after that: parse the raw message, create a
ticket, reply, or file it. A `2xx` response completes the delivery; any
other status or a connection error retries with backoff until the
worker's attempt limit, after which the delivery is marked failed.

The endpoint target is an HTTP(S) URL or the internal DMARC target.
Forwarding an inbound message to another mailbox is a Postal feature that
CamelMailer's route model leaves out for now; to relay inbound mail
onward, point a route at an HTTP endpoint that re-injects it through the
send API.

## What happens to a routed message

Every inbound message is stored the moment it is accepted, so it appears
in the server's messages with its `mail_from`, `rcpt_to`, subject, and
raw content, exactly like an outbound message. The worker then acts on it
according to the route:

- **HTTP endpoint** delivery POSTs the envelope above. A `2xx` completes
  the message; failures retry with backoff and end as a failed delivery
  if the attempts run out.
- **`internal://dmarc-reports`** ingestion records a `Processed` delivery
  entry on success. A message that cannot be parsed as an aggregate
  report is **held** with a delivery entry naming the parse error, so a
  malformed report is visible under the message and never crashes the
  worker.
- **Accept / Hold / Bounce** routes leave the stored message in place with
  nothing to deliver.

Two checks run on inbound mail independently of the route, by inspecting
the message content:

- **Spam and virus inspection.** When rspamd or ClamAV is configured, a
  message that exceeds the spam-failure threshold or fails a virus scan is
  **held** rather than delivered.
- **ARF feedback loops.** A message that an ISP delivers as a
  spam-complaint report (`multipart/report; report-type=feedback-report`)
  is recognised by its envelope and turned into a stream-scoped complaint
  for the recipient who complained, then marked `Processed`.

Bounce-flagged messages (mail arriving at a return path) are classified
into hard, soft, or undetermined so the observability API can break
bounces down by category.

You read inbound messages through the same message endpoints as outbound
mail. `GET /api/v2/server/messages/{id}` returns the message with its
delivery attempts, which is where the `Processed`, `Held`, or failure
entries above show up. See the [Quickstart](quickstart.md) for the
message-reading calls.

## Managing routes

Routes are managed over the admin API with an `X-Admin-API-Key` key or a
user session (`Authorization: Bearer`), and mirrored in the dashboard
under a server's **Routes** tab.

| Method & path | Action |
|---|---|
| `GET /api/v2/admin/organizations/{org}/servers/{server}/routes` | List the server's routes |
| `POST …/routes` | Create a route |
| `GET …/routes/{id}` | Show one route |
| `PATCH …/routes/{id}` | Update `name` and `mode` |
| `DELETE …/routes/{id}` | Delete a route |

A route object looks like this:

```json
{
  "id": 7,
  "uuid": "a1b2c3d4-…",
  "name": "support",
  "token": "9f8e7d…",
  "domain_id": 3,
  "endpoint_url": "https://app.acme.example/inbound",
  "mode": "Endpoint"
}
```

**Create** takes `name` (required, the local part), `domain` (the domain
name; when omitted the route is reachable only by token at
`<token>@<route_domain>`), `mode` (defaults to `Endpoint`), and
`endpoint_url`. The endpoint URL is validated when present: it must be an
HTTP(S) URL or exactly `internal://dmarc-reports`. **Update** changes
`name` and `mode` only; to change the endpoint or domain, delete the
route and create it again.

### In the dashboard

The **Routes** tab lists each route with its local part, mode (as a
badge), and endpoint, filterable by mode. **New route** takes the local
part, a domain, a mode, and (for `Endpoint` mode) the HTTP endpoint URL.
When a server has no routes, the empty state is a reminder that inbound
mail to the server is refused until at least one route exists.

## Examples

**Route replies to your app over HTTP.** Mail to
`support@acme.example` is POSTed to your inbound handler:

```bash
curl -s -X POST \
  "$API/api/v2/admin/organizations/acme/servers/production/routes" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{
    "name": "support",
    "domain": "acme.example",
    "mode": "Endpoint",
    "endpoint_url": "https://app.acme.example/inbound"
  }'
```

Point the MX record for `acme.example` at your CamelMailer `smtp` host and
mail to `support@acme.example` starts flowing to the endpoint.

**Hold mail for manual review.** A route that stores everything addressed
to it without delivering onward:

```bash
curl -s -X POST \
  "$API/api/v2/admin/organizations/acme/servers/production/routes" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"name": "archive", "domain": "acme.example", "mode": "Hold"}'
```

**Ingest DMARC aggregate reports.** The internal target that feeds the
DMARC compliance data:

```bash
curl -s -X POST \
  "$API/api/v2/admin/organizations/acme/servers/production/routes" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{
    "name": "dmarc",
    "domain": "acme.example",
    "endpoint_url": "internal://dmarc-reports"
  }'
```

Then point the `rua=` tag of your DMARC record at that address
(`dmarc@acme.example`). The parsing, storage, and reporting are all
covered in [DMARC monitoring](dmarc.md).

## A note on catch-all

A route answers to an exact local part. The dashboard's **New route**
dialog shows `support or *` as a hint, but the delivery path matches the
recipient's local part exactly, so a route named `*` does not currently
act as a wildcard catch-all. To reach one route from many addresses
today, publish the direct-to-route address `<token>@<route_domain>` and
send to it, or give each address its own route.
