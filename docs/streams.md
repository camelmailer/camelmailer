# Message streams

A message stream is a flat label on a mail server that groups mail of one
class together: your password resets and receipts in one stream, your
newsletter in another, your inbound replies in a third. Every message a
server sends or receives belongs to exactly one stream, and the stream
decides how that mail is treated: which reputation (IP pool) it sends
from, whether marketing rules apply, and how it shows up in reporting.

Streams are plain labels with no hierarchy and no reply-threading. They
live per mail server (each stream references one `server_id`), and every
server starts with one built-in stream created for it automatically:
name **Default Transactional Stream**, permalink `outbound`, type
`transactional`. That stream is the server's `default_stream_id`, so mail
sent without naming a stream lands there.

## The three stream types

`stream_type` is fixed to one of three values (enforced by a database
`CHECK` constraint on the `message_streams` table):

| `stream_type` | Direction | What it is for |
|---|---|---|
| `transactional` | Outbound | One-to-one mail triggered by a user action: password resets, receipts, verification codes, alerts. This is the default and the safe choice for anything a person is waiting on. |
| `broadcast` | Outbound | One-to-many marketing mail: newsletters, announcements, campaigns. Broadcast streams enforce marketing rules (see below). |
| `inbound` | Inbound | A stream that receives mail rather than sending it. Use it to separate inbound processing from your outbound streams. |

The type shapes behaviour on send. A `broadcast` stream turns on the
marketing pipeline described further down; `transactional` and `inbound`
streams send (or receive) without those additions.

## Permalink and name

Each stream carries two identifiers:

- **`permalink`** is the stable, machine-facing identifier. It is unique
  per server (`UNIQUE (server_id, permalink)`) and is what you pass in the
  `stream` field when sending. When you create a stream without giving a
  permalink, one is derived from the name: lower-cased, every
  non-alphanumeric run collapsed to a single hyphen (so `"Product
  Updates"` becomes `product-updates`). **The permalink is immutable.**
  The dashboard shows it in the Edit dialog as a disabled field, and the
  update API has no field to change it, because everything that sends to
  the stream references it.
- **`name`** is the human label shown in the dashboard. You can rename a
  stream at any time without affecting anything that sends to it.

## Per-stream settings

### IP pool (reputation isolation)

A stream can source its outbound mail from a specific IP pool via
`ip_pool_id`. This keeps reputations apart: send your newsletter from one
set of IPs and your receipts from another, so a marketing complaint spike
never drags down the deliverability of a password reset.

At delivery the worker resolves the sending pool in this order:

1. the **stream's** `ip_pool_id`, when set,
2. otherwise the **server's** IP pool,
3. otherwise no pool (default routing).

So a stream with no pool of its own inherits the server's, and a server
with no pool falls through to default routing. Removing a pool detaches
any stream that pointed at it (`ON DELETE SET NULL`) and leaves the
stream on the server pool; the stream itself is never deleted.

In the dashboard the resolved pool appears in the stream header (shown as
*Server default pool* when the stream has none), and you change it in the
stream **Edit** dialog. Broadcast streams in particular should carry a
separate pool. See [Deliverability & IP pools](deliverability.md) for how
pools and addresses are configured.

### Archiving

Set `archived` to hide a stream from day-to-day use. An archived stream
still exists and its historical messages remain readable, but it is
flagged **Archived** in the dashboard and it cannot be a send target: an
explicit send to an archived stream is rejected with `422`
`ValidationError` (`Message stream "…" is archived`). Archiving is
reversible; flip the status back to Active in the Edit dialog, or
`PATCH` the stream with `{"archived": false}`.

Archiving is the way to retire a stream. There is no delete endpoint, and
the permalink is never freed, so a retired stream cannot be recreated
under the same permalink.

## Selecting a stream on send

When you send through `POST /api/v2/server/messages` (or
`.../with_template`), the optional `stream` field names the target stream
by permalink:

```bash
curl -s -X POST "$API/api/v2/server/messages" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{
    "from": "news@acme.example",
    "to": ["subscriber@example.com"],
    "subject": "March newsletter",
    "html_body": "<p>…</p>",
    "stream": "newsletter"
  }'
```

Omit `stream` and the message uses the server's default stream. Name a
stream that does not exist, or one that is archived, and the send is
rejected with `422` `ValidationError`. The full send API, including
templates and attachments, is covered in [Sending email](sending.md).

## Broadcast streams enforce marketing behaviour

Sending on a `broadcast` stream turns on the marketing pipeline, so this
class of mail carries the consent and compliance machinery that
regulators and mailbox providers expect:

- **List-Unsubscribe.** Each broadcast message is built per recipient
  with a one-click RFC 8058 `List-Unsubscribe` header, so every opt-out
  link is unique to that recipient and stream.
- **CAN-SPAM footer.** A footer with an unsubscribe link and the server's
  postal address is appended. The postal address is a server setting;
  broadcast sends need it configured.
- **Opt-in subscribers.** A broadcast stream may only send to addresses
  that have opted in. The send checks every recipient and rejects the
  whole request naming the first address that has not subscribed.
- **Stream-scoped suppressions.** An unsubscribe suppresses the recipient
  on that stream only, so a newsletter opt-out never blocks the same
  person's transactional mail. (Hard bounces and manual suppressions stay
  server-wide.)

The full workflow, subscribers, campaigns and the footer address live in
[Broadcast streams](broadcast.md), and the suppression model in
[Suppressions](suppressions.md).

## Inbound streams receive mail

An `inbound` stream is the receiving counterpart. Rather than being a send
target, it groups mail that arrives at the server so you can process
replies and forwards separately from outbound traffic. Routing incoming
mail to endpoints is described in [Inbound routing](inbound.md).

## Managing streams

Streams are managed through the Server (messaging) API with an
`X-Server-API-Key` credential, and mirrored in the dashboard under a
server's **Streams** tab.

| Method & path | Action |
|---|---|
| `GET /api/v2/server/streams` | List every stream on the server |
| `POST /api/v2/server/streams` | Create a stream |
| `GET /api/v2/server/streams/{permalink}` | Show one stream |
| `PATCH /api/v2/server/streams/{permalink}` | Update name, type, archived, IP pool |
| `POST /api/v2/server/streams/{permalink}/archive` | Archive the stream |

A stream object looks like this:

```json
{
  "id": 4,
  "uuid": "a1b2c3d4-…",
  "name": "Newsletter",
  "permalink": "newsletter",
  "stream_type": "broadcast",
  "archived": false,
  "ip_pool_id": 2
}
```

### Create

`name` is required. `stream_type` defaults to `transactional` and accepts
`transactional`, `broadcast` or `inbound`; an unknown value is rejected
with `422` `ValidationError`. `permalink` is optional and derived from the
name when omitted; a duplicate permalink on the same server is rejected
with `422` `ValidationError`. `ip_pool_id` is optional.

```bash
curl -s -X POST "$API/api/v2/server/streams" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"name": "Newsletter", "stream_type": "broadcast"}'
```

### Update

`PATCH` sends only the fields you want to change. `name`, `stream_type`
and `archived` are updated when present. `ip_pool_id` distinguishes an
omitted key (leave the pool unchanged) from an explicit `null` (clear the
pool, falling back to the server pool) from a value (set the pool):

```bash
# move the stream onto a dedicated pool and rename it
curl -s -X PATCH "$API/api/v2/server/streams/newsletter" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"name": "Weekly Newsletter", "ip_pool_id": 2}'

# detach the stream's pool (back to the server default)
curl -s -X PATCH "$API/api/v2/server/streams/newsletter" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"ip_pool_id": null}'
```

### Archive

```bash
curl -s -X POST "$API/api/v2/server/streams/newsletter/archive" \
  -H "X-Server-API-Key: $SERVER_KEY"
```

Archiving is equivalent to `PATCH {"archived": true}`; unarchive with
`PATCH {"archived": false}`.

### In the dashboard

The **Streams** tab lists every stream with its name, permalink, type and
status, and a **New stream** dialog takes a name and type. Opening a
stream shows its detail page: the header carries the status pill, the
type, the permalink and the resolved IP pool, plus actions to view the
stream's messages, edit it, and archive it. The **Edit** dialog changes
the name, type, status (Active or Archived) and IP pool; the permalink is
shown but disabled. Broadcast streams additionally surface their
subscribers, unsubscribe count and the CAN-SPAM postal-address check on
the same page.
