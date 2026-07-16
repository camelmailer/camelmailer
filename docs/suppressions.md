# Suppressions and complaints

A **suppression** is a standing instruction to stop sending to one
address. It is the safety net that sits in front of delivery: before the
worker hands a message to a remote server, it checks the suppression
list, and a match holds the message. This is how a hard bounce, an
unsubscribe or a spam complaint keeps a bad address from receiving mail
again, automatically and per tenant.

Suppressions are tenant data. The `suppressions` table carries the same
`FORCE ROW LEVEL SECURITY` policy as `messages`: every read and write
runs inside the owning server's tenant context, so one server never sees
another's list. See [Sending email](sending.md) for the send path this
gate protects and [Message streams](streams.md) for the stream model it
builds on.

## The shape of a suppression

Each row records who is blocked, why, and how widely:

| Field | Meaning |
|---|---|
| `address` | The recipient address that is blocked. |
| `type` | A free-text label for the source. The system writes `recipient` (the default), `unsubscribe` and `complaint`. |
| `reason` | Optional human-readable note (for example the bounce diagnostic that prompted the entry). |
| `stream_id` | Scope. `null` blocks every stream on the server; a set id blocks only that one [message stream](streams.md). |

The `type` column defaults to `recipient` in the schema, so a plain
manual block lands as a `recipient` suppression. The other two values are
written by the broadcast opt-out paths described below: a one-click
unsubscribe records an `unsubscribe` row, and a spam complaint records a
`complaint` row.

### About hard bounces

The delivery pipeline classifies every failure. A permanent `5xx` reply
(or an enhanced `5.x.x` status in a bounce DSN) is graded **hard**, a
`4xx` is **soft**, and anything unrecognised is **undetermined**; the
grade is stored on the delivery record so the observability API can break
failures down by category. That classification is where a hard bounce
shows up today. Turning a hard bounce into a standing server-wide
suppression is an operator action: add the address through the admin API
or dashboard (it lands as a server-wide `recipient` row). The schema is
built for exactly this, and the migration that introduced stream scope
names "hard bounces, manual suppressions" as the server-wide case.

## Scope: server-wide vs stream-scoped

The `stream_id` column is what makes suppressions safe to use for
marketing. A server-wide row (`stream_id IS NULL`) blocks the address on
every stream. A stream-scoped row (`stream_id` set) blocks it on that one
stream and leaves all other streams open.

The send gate expresses this in a single condition. An address is
suppressed for a message when a row exists that is either server-wide or
scoped to that message's own stream:

```sql
SELECT count(*) FROM suppressions
 WHERE address = $1 AND (stream_id IS NULL OR stream_id = $2)
```

Here `$2` is the message's `stream_id`. The practical payoff: someone who
unsubscribes from your newsletter gets a suppression scoped to the
broadcast stream, and their password-reset mail on the transactional
stream still sends. A marketing opt-out stays inside marketing.

Because an address can hold both a server-wide row and one or more
stream-scoped rows at once, the uniqueness rule keys on all three parts:

```sql
CREATE UNIQUE INDEX suppressions_server_addr_stream
    ON suppressions (server_id, address, COALESCE(stream_id, 0));
```

`COALESCE(stream_id, 0)` collapses the server-wide row to a stable
sentinel (0 is never a valid stream id), so `(server, address, NULL)` and
`(server, address, 42)` are two distinct, allowed rows. The same address
can be blocked everywhere and additionally opted out of one stream.

## How the worker applies the gate

The delivery worker checks the list first thing when it picks up a
queued message, passing the message's own `stream_id` so the scope rule
applies:

```rust
if ServerStore::address_suppressed(
    &self.store, message.server_id, &message.rcpt_to, message.stream_id,
).await? {
    // held: complete the queue entry, record the delivery, fire the webhook
}
```

On a match the worker does three things: it completes the queue entry so
the message is not retried, it records a delivery with status `Held` and
the note `recipient is on the suppression list`, and it fires the
`MessageHeld` webhook. The message never reaches a remote server. A held
message is visible under the message in the dashboard and API with that
delivery note, so a held send is easy to explain.

## Feedback loops (FBL / ARF)

Large mailbox providers offer feedback loops: when a recipient marks your
mail as spam, the provider sends you a complaint report in the ARF format
(RFC 5965), an ordinary email whose body is a `multipart/report` with
`report-type=feedback-report`. CamelMailer ingests these automatically.

The worker recognises an ARF report by its envelope, independent of how
it was routed, and hands it to the complaint path instead of delivering
it. From there:

- The report is parsed. A report that cannot be parsed is **held**, with
  the parse error recorded on the delivery, so a malformed report never
  crashes the worker.
- An **abuse** report that maps back to a broadcast recipient (through the
  `List-Unsubscribe` token embedded in the original message) becomes a
  `complaint`. This records a stream-scoped `complaint` suppression for
  that recipient **and** flips their subscription on that stream to
  `unsubscribed`. The delivery is marked `Processed` with the note "spam
  complaint recorded for the broadcast recipient".
- A report that parses but has nothing to action (a non-abuse feedback
  type, or no recoverable recipient token) is recorded as `Processed` and
  taken no further.
- A storage error while recording the complaint is treated as transient
  and retried with backoff, so a complaint is not lost to a blip.

The subscription flip is why a complaint on a broadcast subscriber does
double duty: the `complaint` suppression stops future sends on that
stream, and the `unsubscribed` status removes the address from the
stream's audience so a future campaign never re-adds it. See
[Broadcast streams](broadcast.md) for the subscription model this updates.

**Honest note for local development.** ARF ingestion runs inside the
delivery worker on inbound mail, so it only happens when a report
actually arrives as inbound email. A local stack that has no inbound
route and no MX pointed at it never receives an ARF report, so this path
stays dormant in dev. To exercise complaints locally, use the manual
complaint endpoint below, which takes the same `record_complaint` path
without needing a real feedback loop.

### Recording a complaint by hand

The same complaint outcome (stream-scoped `complaint` suppression plus a
subscription flip to `unsubscribed`) is available directly on the Server
API. It is idempotent, so recording the same complaint twice is safe:

```bash
curl -s -X POST \
  "$API/api/v2/server/streams/newsletter/subscribers/reader@example.com/complaint" \
  -H "X-Server-API-Key: $SERVER_KEY"
```

## Managing suppressions in the dashboard

Each server has a **Suppressions** tab. The table lists every entry with
these columns:

| Column | Shows |
|---|---|
| Address | The blocked address (links to the recipient view). |
| Type | The `type` label as a badge; filterable, so you can isolate all `complaint` or `unsubscribe` entries. |
| Scope | `All streams` for a server-wide row, or the stream's name for a stream-scoped one. |
| Reason | The stored note. |
| Date added | When the row was created. |

The **Scope** column is the stream-aware view of the list: it reads each
row's `stream_id` and names the stream, so you can see at a glance whether
an entry blocks the whole server or just one stream. The list response
carries the server's streams alongside the suppressions for exactly this,
so the management surface can name a scope without a second call.

Adding an address from the dashboard creates a server-wide `recipient`
suppression. The table exports to CSV, and removing a row reactivates the
address so the server delivers to it again.

## Managing suppressions via the API

Suppressions live on the **Management (Admin) API**
(`/api/v2/admin/...`), authenticated with `X-Admin-API-Key` or a user
session. The examples reuse the `$API`, `$ADMIN_KEY` and org/server
permalinks from the [quickstart](quickstart.md).

### List

```bash
curl -s \
  "$API/api/v2/admin/organizations/acme/servers/production/suppressions" \
  -H "X-Admin-API-Key: $ADMIN_KEY"
```

The response `data` holds `suppressions` (each with `type`, `address`,
`reason` and `stream_id`), the server's `streams` for naming each scope,
and `pagination` (`page` / `per_page`, capped at 100).

### Add

`POST` to the same path. Only `address` is required; `type` defaults to
`recipient` and `reason` is optional. Entries added here are server-wide
(`stream_id` is `null`); stream-scoped rows come from the unsubscribe and
complaint paths.

```bash
curl -s -X POST \
  "$API/api/v2/admin/organizations/acme/servers/production/suppressions" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"address": "bounced@example.com",
       "type": "recipient",
       "reason": "550 5.1.1 mailbox does not exist"}'
```

### Delete

```bash
curl -s -X DELETE \
  "$API/api/v2/admin/organizations/acme/servers/production/suppressions/bounced@example.com" \
  -H "X-Admin-API-Key: $ADMIN_KEY"
```

Deleting is keyed on the address alone, so it clears every suppression for
that address on the server, both the server-wide row and any stream-scoped
ones. After deletion the address is deliverable again, so resolve the
underlying bounce or complaint before you remove the entry.

## Related

- [Sending email](sending.md): the send path this gate guards.
- [Message streams](streams.md): the stream model that scopes suppressions.
- [Broadcast streams](broadcast.md): subscriptions and one-click unsubscribe.
- [DMARC monitoring](dmarc.md): the sibling inbound-report pipeline (aggregate reports rather than complaints).
