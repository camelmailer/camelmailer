# Sending email

Sending is the core of Camelmailer. You hand a message to the platform,
it stores and queues it, and the delivery worker takes it from there. There
are two front doors into that same pipeline: an HTTP send API and SMTP
submission. Both authenticate with a server-scoped credential, both apply
the same From-address rules, and both end in one stored, queued message per
recipient.

This page covers both doors, the request and response shapes, attachments
and custom headers, how the [message stream](streams.md) is chosen, and
what actually happens after a message is accepted.

## The HTTP send API

The send endpoint lives on the Messaging surface (base path
`/api/v2/server`) and authenticates with a server API credential passed in
the `X-Server-API-Key` header. That credential is an `API`-type credential
of one mail server (see [Quickstart step 4](quickstart.md)); it scopes the
request to that server and its data.

| Endpoint | Purpose |
|---|---|
| `POST /api/v2/server/messages` | Send one message |
| `POST /api/v2/server/messages/batch` | Send an array of messages in one call |
| `POST /api/v2/server/messages/with_template` | Render a stored [template](templates.md), then send |
| `POST /api/v2/server/messages/with_template/batch` | Render and send to many recipients |

### Request payload

The body is JSON. Only `from` and at least one recipient are required;
everything else is optional.

| Field | Type | Notes |
|---|---|---|
| `from` | address | **Required.** The sender. Its domain must be authorized for this server (see below). |
| `to` | array of addresses | Primary recipients. |
| `cc` | array of addresses | Carbon-copy recipients. |
| `bcc` | array of addresses | Blind carbon-copy recipients. |
| `reply_to` | array of addresses | Sets the `Reply-To` header. |
| `subject` | string | Message subject. Defaults to empty when omitted. |
| `html_body` | string | HTML part. |
| `text_body` | string | Plain-text part. |
| `headers` | object (string to string) | Extra headers such as `X-*` or `List-*`. Reserved names are ignored (see [Custom headers](#custom-headers)). |
| `attachments` | array of attachments | See [Attachments](#attachments). |
| `tag` | string | Free-form label stored with the message and available as a filter in the message list, stats and tags endpoints. |
| `metadata` | object | Arbitrary JSON stored alongside the message. |
| `stream` | string | Permalink of the target [message stream](streams.md). Defaults to the server's default stream. |

At least one of `to`, `cc` or `bcc` must be present and non-empty. A
request with only a `from` returns `400 ParameterMissing`.

An **address** is either a bare email string or an object carrying a
display name:

```json
"to": [
  "ada@example.com",
  { "email": "grace@example.com", "name": "Grace Hopper" }
]
```

You can supply `html_body`, `text_body`, or both. With both parts a
`multipart/alternative` message is built. With neither, an empty text part
is written so the message stays valid.

### From-address authorization

The `from` address is checked before anything is queued. The request is
accepted when either of these holds:

- the server (or its organization) owns a **verified sending domain** that
  matches the From domain, or
- the exact From address is a **confirmed sender address** of the server.

When the domain matches a verified domain, that domain's DKIM key signs the
message. A From address that satisfies neither rule returns
`422 ValidationError`. SMTP submission applies the same two-step rule inside
the session state machine, so the behavior is identical on both doors.

### Response shape

Every Messaging response uses the standard envelope
`{ status, time, data }` (or `{ status, time, error }` on failure), where
`time` is the server-side processing time in seconds. A successful send
returns `201 Created`:

```json
{
  "status": "success",
  "time": 0.012,
  "data": {
    "message_id": 4021,
    "recipients": [
      { "rcpt_to": "ada@example.com", "message_id": 4021, "token": "b1f9c3e2d0", "status": "queued" },
      { "rcpt_to": "grace@example.com", "message_id": 4022, "token": "7a2c58f11e", "status": "queued" }
    ]
  }
}
```

One entry appears per recipient across `to`, `cc` and `bcc`, because each
recipient becomes its own stored message. The top-level `message_id` is the
numeric id of the first stored message, a convenience for the common
single-recipient send.

Error responses carry a stable `error.code` (for example `ParameterMissing`
or `ValidationError`) and a human-readable `error.message`.

### Batch sends

`POST /api/v2/server/messages/batch` takes an array of send requests and
returns one result per entry, so a single bad entry reports its own error
while the others still queue:

```json
{
  "status": "success",
  "time": 0.031,
  "data": {
    "messages": [
      { "status": "success", "data": { "message_id": 4023, "recipients": [ … ] } },
      { "status": "error", "error": { "code": "ValidationError", "message": "…" } }
    ]
  }
}
```

### Idempotent retries

Add an `Idempotency-Key` header when a send may be retried after a timeout or
lost response:

```bash
-H "Idempotency-Key: receipt/order-10432"
```

The key must be 1–256 characters and is scoped to the authenticated server.
Camelmailer retains a completed result for 24 hours:

- retrying the same endpoint and normalized request with the same key returns
  the original success response, including the original message ids and
  tokens, without queueing another message;
- reusing the key for another payload or send endpoint returns
  `409 InvalidIdempotentRequest`;
- an empty, repeated, or overlong header returns
  `400 InvalidIdempotencyKey`;
- a batch key covers the whole batch, including its per-item success and error
  results.

The 256-character limit applies to the caller-provided key before it is
hashed. Camelmailer stores only SHA-256 hashes of the key and normalized
request, but validating the external value first keeps the public protocol
bounded and predictable. Key length therefore does not affect the database
index size.

Generate a key for the application operation you are protecting—for example,
`receipt/order-10432`. Do not use the email's RFC `Message-ID` as the
idempotency key. `Message-ID` identifies a MIME message; an idempotency key
identifies a send API operation and must be available before Camelmailer
creates and queues that message.

### How message IDs work

Three separate identifiers travel with a message, and it helps to keep them
apart:

| Identifier | Where it appears | What it is |
|---|---|---|
| `message_id` (integer) | send response, `GET /messages/{id}` path | The numeric database id of a stored message. Use it to read the message back. |
| `token` | send response, message record | An opaque per-message token, also used to build tracking and share links. |
| `message_id` (string) | `GET /messages/{id}` response | The RFC 5322 `Message-ID` header, a string like `<…@host>`. |

The RFC `Message-ID` header is generated automatically when the MIME
message is built. On the read side, `GET /api/v2/server/messages/{id}`
returns it as the `message_id` field of the message record, while the send
response `message_id` is the numeric id you pass back into the path. When
you read a message, the numeric id is in the URL and the header string is in
the body.

### curl example

```bash
curl -s -X POST "$API/api/v2/server/messages" \
  -H "X-Server-API-Key: $SERVER_KEY" \
  -H "Idempotency-Key: receipt/order-10432" \
  -H "Content-Type: application/json" \
  -d '{
    "from": { "email": "billing@acme.example", "name": "Acme Billing" },
    "to": ["ada@example.com"],
    "reply_to": ["support@acme.example"],
    "subject": "Your receipt",
    "html_body": "<p>Thanks for your purchase.</p>",
    "text_body": "Thanks for your purchase.",
    "tag": "receipt",
    "headers": { "X-Order-Id": "10432" },
    "attachments": [
      { "name": "invoice.pdf", "content_type": "application/pdf",
        "data_base64": "JVBERi0xLjQK…" }
    ]
  }'
```

## Attachments

Each attachment is an object with three required fields:

| Field | Type | Notes |
|---|---|---|
| `name` | string | The filename, used as the attachment's `Content-Disposition` name. |
| `content_type` | string | MIME type, for example `application/pdf` or `image/png`. |
| `data_base64` | string | The file content, base64-encoded (standard alphabet). |

Content that fails to decode as base64 returns `422 ValidationError`
naming the offending attachment, and nothing is queued. Total message size
is bounded by the SMTP server's `max_message_size` for anything that
transits SMTP, so keep large payloads in mind (see
[Configuration](configuration.md)).

## Custom headers

The `headers` object adds arbitrary headers to the outgoing message, which
is the place for things like `X-*` application markers or `List-*` headers.
The MIME builder owns a set of reserved headers and ignores any client
value for them, so identity and routing headers cannot be forged:

```
from, to, cc, bcc, reply-to, subject, date,
message-id, content-type, content-transfer-encoding, mime-version
```

To set the sender, recipients, subject or reply address, use the dedicated
payload fields; the reserved list above shows which names `headers` skips.

## Choosing a message stream

Every send is attributed to one [message stream](streams.md). Streams
separate traffic classes (for example transactional versus broadcast) so
they get their own tracking, stats and configuration.

- Omit `stream` and the message is attributed to the server's default
  stream.
- Set `stream` to a stream permalink to target that stream. The stream must
  exist and must not be archived, otherwise the send returns
  `422 ValidationError`.

**Broadcast streams add behavior on send.** When the target stream is a
broadcast stream, Camelmailer requires every recipient to have opted in to
that stream and rejects the whole request naming the first address that has
not. It also gives each recipient a unique one-click unsubscribe: per-recipient
`List-Unsubscribe` and `List-Unsubscribe-Post` headers (RFC 8058) plus a
visible unsubscribe footer, and the sender's physical postal address when
one is configured on the server. Transactional and inbound streams are
unaffected by all of this. See [Broadcast streams](broadcast.md) for the
opt-in model and unsubscribe handling, and [Suppressions](suppressions.md)
for how opt-outs are recorded.

## SMTP submission

SMTP submission is the alternative front door, for clients and libraries
that already speak SMTP. It reaches the same pipeline as the HTTP API and
applies the same From-address authorization.

**Credentials.** Create a credential of type `SMTP` on the server (the HTTP
API uses `API`-type credentials; SMTP submission uses `SMTP`-type ones):

```bash
curl -s -X POST "$API/api/v2/admin/organizations/acme/servers/production/credentials" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"type": "SMTP", "name": "smtp-submission"}'
```

The returned `data.credential.key` is the SMTP password. Authenticate with
`AUTH PLAIN` or `AUTH LOGIN`, passing that key as the password; the username
is accepted but not used for these mechanisms. The credential identifies the
server on its own.

**STARTTLS.** The server advertises `STARTTLS` until the session is
upgraded, and it advertises `AUTH` only once the session is TLS-protected.
The AUTH command itself is refused on the same condition, with
`538 5.7.11 Encryption required for requested authentication mechanism`, so a
client that skips STARTTLS and sends AUTH anyway is turned away before its
credential reaches the wire. When `smtp_server.tls_enabled` is off the server
has no STARTTLS to offer, and AUTH stays available on the plain session for
deployments that terminate TLS elsewhere.
Port 25 speaks plain SMTP with STARTTLS; a listener on 587 is the usual
submission port, and a listener on 465 uses implicit TLS from the first byte.
Ports and certificates are set in `smtp_server` (see
[Configuration](configuration.md)).

**Example session** over the submission port with `swaks`:

```bash
swaks --server mx.example.com --port 587 --tls \
  --auth PLAIN --auth-user ignored --auth-password "$SMTP_KEY" \
  --from billing@acme.example \
  --to ada@example.com \
  --header "CamelMailer-Idempotency-Key: receipt/order-10432" \
  --header "Subject: Your receipt" \
  --body "Thanks for your purchase."
```

A successful `AUTH` is acknowledged with `235 Granted for <org>/<server>`.
As with the HTTP API, each `RCPT TO` recipient becomes its own stored,
queued message.

For retry-safe authenticated submission, add exactly one
`CamelMailer-Idempotency-Key` message header. It follows the same 1–256
character and 24-hour rules as the HTTP header. The request fingerprint covers
the envelope sender, the ordered envelope recipients and the client-supplied
message data. A replay receives `250 OK` without another queued message; reuse
with different content or recipients receives `554`. Camelmailer removes this
operational header before storing and delivering the message. It is not
accepted on unauthenticated inbound mail.

**Managing credentials in the dashboard.** A server's **Credentials** tab
lists every credential with its name, type and status. Opening one shows a
details lightbox rather than a separate page: its status (Active or On
hold), and for an API credential the key, for an `SMTP-IP` credential the
allowed CIDR, and for an `SMTP` credential the copy-first connection facts
(host, the submission ports, the username `org/server`, and the password,
which is the credential's key). Export from this tab carries credential
metadata only; the secret key is shown once at creation and is never
exported (see [Import and export](import-export.md)).

## Send limits

A mail server can carry a **send limit**: the number of outgoing messages it
may send in the trailing 30 days, meaning today plus the 29 days before it.
A server without one sends without a cap, which is how every server starts.

The limit is the operator's control over a tenant, so only a global
administrator or a machine key can set it. An organization owner sees the
limit on the server's settings page and cannot change it, which is the point:
a tenant that could raise its own limit has no limit.

Set it through the admin API, or in the dashboard under a server's settings
when signed in as a global administrator:

```bash
curl -X PATCH https://api.camelmailer.com/api/v2/admin/organizations/acme/servers/mail \
  -H "X-Admin-API-Key: $ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"send_limit": 5000}'
```

Send `{"send_limit": null}` to make the server unlimited again. Leaving the
field out of the request leaves the current limit alone, so an unrelated
settings change never clears it by accident.

### What happens when a server is full

Both submission paths refuse before anything is stored, so a refused send
consumes nothing:

| Path | Response |
|---|---|
| HTTP send API | `429` with the error code `SendLimitExceeded` |
| SMTP end of `DATA` | `550 5.7.1` naming the limit and the window |

SMTP accepts the envelope so it can read the idempotency key in DATA. A
completed keyed retry returns `250 OK` even when the quota is full. New
submissions are checked against current usage after DATA, before storage.

SMTP answers `5xx` rather than `4xx` on purpose. The window is 30 days, so a
transient code would have the sending client retrying for weeks.

A request naming more recipients than the remainder is refused whole rather
than partly accepted. Queuing the recipients that fit and dropping the rest
would leave the caller unable to tell which ones went.

### What counts

Every outgoing message counts once, per recipient, whichever path created it:
the send API, SMTP submission, a broadcast to a stream, a campaign expanded to
its subscribers, and platform mail such as password resets. Inbound mail does
not count, so a busy inbound route never exhausts the outbound quota, and
neither does an imported message, since importing history is not sending.

Usage is counted in daily buckets rather than by counting stored messages.
That way `message_retention_days` deleting old messages cannot hand a server
its quota back before the window has rolled forward. The worker's hourly
housekeeping drops buckets once they have left every window.

## What happens after a message is accepted

Accepting a message and delivering it are two steps:

1. **Acceptance.** The request is validated, the From address is
   authorized, the stream is resolved, the raw MIME is built, and one
   message per recipient is stored with status `queued`. The API responds at
   this point. Acceptance does not mean the mail has left the building.
2. **Delivery.** The worker dequeues messages (using PostgreSQL
   `SKIP LOCKED`, so any number of workers cooperate safely), signs them
   with DKIM, rewrites tracking links where enabled, and delivers either
   direct-to-MX on port 25 or through the configured `smtp_relays`. Delivery
   attempts and their outcomes are recorded against the message and readable
   via `GET /api/v2/server/messages/{id}/deliveries`.

Sends without an idempotency header retain the normal behavior: submitting the
same request again queues another message. Use a unique operation key whenever
the caller may retry an uncertain result.

### Local development without a worker

In a local stack that runs only the web and SMTP processes, sends succeed
and messages are stored with status `queued`, but nothing delivers them:
the queue simply grows. Start the `worker` process to move queued messages
out the door. The full Docker Compose stack in the
[Quickstart](quickstart.md) already includes the worker, so messages there
are delivered (direct-to-MX, or via relays when configured).
