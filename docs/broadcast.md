# Broadcast (marketing) streams

A **broadcast stream** is a [message stream](streams.md) whose `stream_type`
is `broadcast`. It carries your marketing mail: newsletters, product
announcements, the weekly digest. Where a transactional stream ships a
receipt or a password reset to someone who is expecting it, a broadcast
stream ships promotional mail to a list, so it has to answer to a stricter
set of rules.

CamelMailer enforces four of them, and it enforces them **only** for
`broadcast` streams. A `transactional` or `inbound` stream behaves exactly
as it always has; every broadcast rule below is gated on
`stream_type == "broadcast"`.

| Guarantee | What the broadcast stream does |
|---|---|
| One-click unsubscribe | Every message carries an RFC 8058 `List-Unsubscribe` header pointing at a public opt-out endpoint |
| Stream-scoped opt-out | An unsubscribe suppresses the recipient **on this stream only**, so it never blocks their transactional mail |
| CAN-SPAM footer | A visible unsubscribe link plus your physical postal address is appended to the body |
| Opt-in enforcement | A broadcast may only be sent to an address that has an active `subscribed` record on the stream |

Everything a broadcast stream does is bound to the stream. That is the
mental model to hold onto: **a subscriber is a per-stream consent record
wired to the stream, and an unsubscribe is a per-stream opt-out wired to the
stream.** Neither is a global property of an email address.

---

## The mental model: subscribers are wired to the stream

A subscriber is not a contact in a global address book. It is a row in the
`subscriptions` table keyed by `(server_id, stream_id, address)` with a
`status` of `subscribed` or `unsubscribed`. Consent belongs to one specific
stream on one specific server.

```
subscriptions
  id           BIGSERIAL PRIMARY KEY
  server_id    BIGINT  -> servers(id)
  stream_id    BIGINT  -> message_streams(id)      -- always set
  address      TEXT
  status       TEXT    CHECK (status IN ('subscribed','unsubscribed'))
  created_at   TIMESTAMPTZ DEFAULT now()
  UNIQUE (server_id, stream_id, address)
```

Two consequences fall out of the key:

- The **same address can be subscribed to your `newsletter` stream and
  absent from your `product-updates` stream.** They are independent rows.
- The table is protected by the same **row-level security** as `messages`:
  it enables and forces RLS, and its tenant policy filters on
  `current_setting('camelmailer.server_id')`. A subscriber list is only ever
  visible inside the owning server's tenant context.

Suppressions are wired to the stream in the same way. A row in
`suppressions` now carries a nullable `stream_id`:

- `stream_id IS NULL` is a **server-wide** suppression. Hard bounces and
  manual suppressions land here, and they block delivery on every stream.
  This is the historical behaviour, unchanged.
- `stream_id = <id>` is a **stream-scoped** suppression. A marketing
  unsubscribe lands here and blocks this one stream.

A recipient is blocked for a given message when a suppression exists with
`stream_id IS NULL` **or** `stream_id = message.stream_id`. That single rule
is what lets a marketing opt-out coexist with a fully deliverable
transactional relationship. The uniqueness key became
`(server_id, address, COALESCE(stream_id, 0))`, so one address can hold both
a server-wide row and a per-stream row at once (0 stands in for the
server-wide sentinel, since it is never a real stream id).

---

## 1. One-click unsubscribe

Every message sent on a broadcast stream carries two headers, built per
recipient at send time and baked into the stored raw message:

```
List-Unsubscribe: <https://track.example.com/track/u/AbC123…>, <mailto:unsubscribe@track.example.com>
List-Unsubscribe-Post: List-Unsubscribe=One-Click
```

The `https://` URL is `{web_protocol}://{track_domain}/track/u/{token}`,
where `track_domain` is the same tracking host the worker uses for open and
click tracking (see [configuration.md](configuration.md)). The
`List-Unsubscribe-Post` header is the RFC 8058 signal that lets a mailbox
provider (Gmail, Apple Mail, Yahoo) render a native "Unsubscribe" button and
POST to the endpoint on the recipient's behalf, with no page visit.

Each recipient gets their **own** token and therefore their own raw message,
so every opt-out link resolves to exactly one address on one stream. The
token is registered in `enqueue_send` before the message is queued (it has to
exist before delivery, just like open and click tokens are pre-registered).

### The public endpoint

```
GET  /track/u/{token}     browser click     -> HTML confirmation page
POST /track/u/{token}     one-click / RFC 8058 -> empty 200
```

This endpoint is **public and unauthenticated**: the opt-out link travels to
strangers, so it carries no tenant context. It resolves the opaque token
alone through a cross-tenant lookup table (`unsubscribe_tokens`), the same
pattern the open/click tracking endpoints use. Because it is served through
`ServerStore`, which both the Postgres and in-memory stores implement, it
works regardless of the tracking backend.

Recording an unsubscribe does three things under the resolved tenant:

1. writes a **stream-scoped suppression** of type `unsubscribe`, with reason
   `Unsubscribed via List-Unsubscribe`, scoped to the token's `stream_id`,
2. flips the matching `subscriptions` row to `unsubscribed`,
3. returns without leaking whether the token was valid.

It is **idempotent**. A duplicate stream-scoped suppression is treated as a
success, not an error, so a mailbox provider that POSTs twice, or a recipient
who clicks and then also gets an automated POST, produces one clean opt-out.

The response is deliberately content-free about token validity. `GET` always
returns the same neutral confirmation page whether or not the token matched:

```html
<!doctype html><html><head><meta charset="utf-8"><title>Unsubscribed</title></head>
<body><p>You have been unsubscribed.</p></body></html>
```

`POST` returns an empty `200`. Neither verb reveals whether an address is on
your list.

### Try it

```bash
# One-click (what a mailbox provider sends):
curl -s -X POST "https://track.example.com/track/u/AbC123…"
# -> 200, empty body

# Browser click (what a recipient sees):
curl -s "https://track.example.com/track/u/AbC123…"
# -> 200, the "You have been unsubscribed." page
```

After either call, the address is suppressed on that stream and its
subscription reads `unsubscribed`. Sending the same broadcast again holds the
message; sending that address on a **transactional** stream still delivers
(that is the whole point, and it is covered in step 4 of the walkthrough
below).

---

## 2. Reputation isolation

Marketing mail and transactional mail earn different reputations, and you do
not want a promotional campaign's complaint rate weighing on the IP that
delivers your password resets. A broadcast stream can therefore send from its
own IP pool.

Each stream carries a nullable `ip_pool_id`. When the worker picks a source
IP for a message it resolves, in order: the **stream's** pool, then the
**server's** pool, then none. A broadcast stream with its own pool sends from
dedicated addresses; a stream that leaves `ip_pool_id` unset inherits the
server default, so nothing changes for streams you have not configured.

Set the pool on the stream from the dashboard (the broadcast stream's detail
page has an IP-pool selector) or when creating and updating the stream via
the API. Pools themselves are an admin-API resource. For how pools, warm-up,
and per-pool reputation fit together, see
[Deliverability & IP pools](deliverability.md).

---

## 3. CAN-SPAM compliance footer

CAN-SPAM requires marketing mail to carry a visible way to opt out and the
sender's physical postal address. CamelMailer appends both to every broadcast
message, in **both** the HTML and the plain-text body, before the raw message
is built. The footer travels with the stored message exactly like the
`List-Unsubscribe` header.

The HTML footer is a small styled block:

```html
<div style="margin-top:24px;padding-top:16px;border-top:1px solid #e5e5e5;
     font-family:Arial,sans-serif;font-size:12px;color:#8a8a8a;">You are
  receiving this because you subscribed.
  <a href="https://track.example.com/track/u/AbC123…" style="color:#8a8a8a;">Unsubscribe</a>.<br>
  Acme Inc, 123 Main St, Berlin
</div>
```

The plain-text footer mirrors it:

```
--
Unsubscribe: https://track.example.com/track/u/AbC123…
Acme Inc, 123 Main St, Berlin
```

The unsubscribe link in the footer reuses this recipient's one-click token,
so the visible link and the header point at the same place.

### The postal address

The address comes from `servers.broadcast_physical_address`, a server-level
setting. Set it under **Server → Settings → Broadcast postal address** in the
dashboard, or via the server settings API.

When the address is **unset**, the footer is still appended (with the
unsubscribe link, without an address line), and the dashboard raises a
compliance warning on the broadcast stream's detail page:

> No broadcast postal address is set (required for CAN-SPAM).

The warning links straight to Settings. Set the address before you run real
campaigns: an unsubscribe link alone does not satisfy CAN-SPAM.

---

## 4. Opt-in enforcement

A broadcast may only be sent to an address that has consented. The consent
record is the `subscriptions` row described above.

### The send gate

At enqueue time, a broadcast send checks every recipient with
`is_subscribed(server_id, stream_id, address)`, which is true only when a row
exists with `status = 'subscribed'`. If **any** recipient is not subscribed,
the whole request is rejected with **`422 Unprocessable Entity`**, naming the
first offender:

```json
{
  "status": "error",
  "error": {
    "code": "ValidationError",
    "message": "someone@example.com has not opted in to the newsletter stream"
  }
}
```

Transactional and inbound streams have no such gate. This check runs only for
`broadcast`.

### Managing subscribers

Subscribers live under the messaging API, per stream, authenticated with a
[Server API key](authentication.md):

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/v2/server/streams/{permalink}/subscribers` | List subscribers and their status |
| `POST` | `/api/v2/server/streams/{permalink}/subscribers` | Add or update one `{address, status?}` (status defaults to `subscribed`) |
| `POST` | `/api/v2/server/streams/{permalink}/subscribers/import` | Bulk-upsert `{addresses: […]}` as `subscribed` |
| `DELETE` | `/api/v2/server/streams/{permalink}/subscribers/{address}` | Remove a subscriber row entirely |
| `POST` | `/api/v2/server/streams/{permalink}/subscribers/{address}/complaint` | Flip to `unsubscribed` and write a stream-scoped `complaint` suppression |

Adding is an **upsert**: posting an address that already exists updates its
status rather than erroring. Import skips blanks and de-duplicates within the
request, and returns how many rows it touched plus the resulting count:

```json
{ "added": 240, "total": 1180 }
```

The dashboard surfaces all of this on the broadcast stream's detail page: an
add box, a paste-to-import textarea, the subscriber list with a status badge,
and per-row "Mark complaint" and "Remove" actions.

### Consent closes automatically

You rarely flip consent by hand. Two events close it for you, each writing a
stream-scoped suppression **and** setting the subscription to `unsubscribed`:

- **An unsubscribe** (the `/track/u/{token}` endpoint above) writes an
  `unsubscribe` suppression.
- **An automatic FBL/ARF complaint.** When a mailbox provider forwards a spam
  complaint as an ARF feedback-loop report, the worker parses it, and for an
  abuse report that maps back to a broadcast recipient (via the original
  message's `List-Unsubscribe` token) it records a `complaint` suppression
  with reason `Spam complaint (feedback loop)` and flips consent closed. A
  report it cannot parse or cannot map to a recipient is held rather than
  dropped. See [Suppressions](suppressions.md) for how bounces, unsubscribes,
  and complaints all land on the suppression list and how scope is displayed.

Either way, the address stops receiving this stream's mail and keeps
receiving its transactional mail.

---

## End-to-end walkthrough

This assumes the [quickstart](quickstart.md) stack is running and you have a
`$SERVER_KEY` and `$API` exported. Create a broadcast stream (or reuse a
seeded one), then:

```bash
# 1. Create the broadcast stream
curl -s -X POST "$API/api/v2/server/streams" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"name": "Newsletter", "stream_type": "broadcast"}'

# 2. Opt a recipient in
curl -s -X POST "$API/api/v2/server/streams/newsletter/subscribers" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"address": "reader@example.com"}'

# 3. Send the broadcast (the `stream` field selects the broadcast stream)
curl -s -X POST "$API/api/v2/server/messages" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{
    "from": "news@acme.example",
    "to": ["reader@example.com"],
    "stream": "newsletter",
    "subject": "This week at Acme",
    "html_body": "<p>Hello!</p>",
    "text_body": "Hello!"
  }'
```

Read the stored message back (the raw is on the message's HTML/Raw tab in the
dashboard, or via `GET /api/v2/server/messages/{id}`): it carries the
`List-Unsubscribe` header and the compliance footer.

Send the same broadcast to an address that has **not** opted in and the
request is rejected before anything is queued:

```bash
curl -s -X POST "$API/api/v2/server/messages" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"from":"news@acme.example","to":["stranger@example.com"],
       "stream":"newsletter","subject":"x","text_body":"x"}'
# -> 422 ValidationError: "stranger@example.com has not opted in to the newsletter stream"
```

Now hit the recipient's unsubscribe link (copy the token from the stored
message's `List-Unsubscribe` header):

```bash
curl -s -X POST "https://track.example.com/track/u/<token>"
```

The address is now suppressed **on the newsletter stream**. Re-sending the
broadcast to that address holds the message. Sending that same address on a
**transactional** stream still delivers, which is the guarantee the whole
design exists to provide.

For a first-class, tracked send to your whole audience with a
draft/schedule/send-now lifecycle rather than a hand-rolled loop, use
[Campaigns](campaigns.md); a campaign expands into one broadcast message per
subscriber through this exact path.

---

## Local-dev limitations

Be honest with yourself about what a local stack does and does not exercise:

- **No delivery worker means mail is not actually delivered.** The web
  process stores and queues the message, but without a running `worker` it is
  never handed to an MX. Everything up to and including the stored raw (the
  `List-Unsubscribe` header, the footer, the opt-in gate, the suppression) is
  fully exercisable; the send itself is not.
- **Opens and ARF complaints are not live locally.** Feedback-loop reports
  arrive as real inbound mail from mailbox providers, so the automatic
  complaint path (step 4) does not fire on a laptop. You can still simulate
  the outcome directly: the `subscribers/{address}/complaint` endpoint writes
  the same `complaint` suppression and flips the same consent row.
- The unsubscribe endpoint itself **does** work locally: it only needs the
  web process and the store, so `POST /track/u/{token}` records the opt-out
  end to end.

---

## See also

- [Message streams](streams.md): stream types, defaults, archiving.
- [Campaigns](campaigns.md): planned, tracked sends to a broadcast audience.
- [Suppressions](suppressions.md): bounces, unsubscribes, complaints, and scope.
- [Deliverability & IP pools](deliverability.md): reputation, pools, warm-up.
- [Sending email](sending.md): the messaging API in full.
