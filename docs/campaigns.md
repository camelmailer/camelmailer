# Campaigns: planning and sending a broadcast

A **campaign** is a first-class record of one broadcast: a single piece
of content sent to the subscribers of a [broadcast stream](broadcast.md).
You compose it once (subject, From address, HTML and text body), point it
at a broadcast stream, and CamelMailer expands it into one message per
subscriber. Each of those messages carries the campaign's `campaign_id`,
so the per-campaign analytics roll up over exactly the mail that campaign
produced.

Campaigns live on the **Server API** (`X-Server-API-Key`, base path
`/api/v2/server`), the same credential you use for
[message streams](streams.md) and one-off sends. They are tenant data:
row-level security scopes every read and write to the owning server, the
same isolation the `messages` table uses.

## How a campaign relates to a stream and its subscribers

A campaign always targets one **broadcast** stream. Transactional streams
are for one-off, per-recipient sends and cannot host a campaign; the API
returns `422 ValidationError` (`campaigns can only target a broadcast
stream`) if you point one at a transactional stream.

The audience is the stream's opted-in subscribers: the addresses with a
`subscribed` subscription row for that stream. See
[Broadcast streams](broadcast.md) for how subscriptions and consent work.
When a campaign sends, it walks the current `subscribed` addresses and
skips everyone else, so an unsubscribe recorded before the send takes the
recipient out of that campaign.

## Fields

| Field | Type | Meaning |
|---|---|---|
| `id` | integer | Campaign id, unique within the server. |
| `stream_id` | integer | The broadcast stream that supplies the audience. |
| `stream` | object | On server-level list/detail: the audience stream's `permalink` and `name` (may be `null` if the stream was deleted). |
| `name` | string \| null | Internal label for the campaign. |
| `subject` | string \| null | The message subject. |
| `from` | string \| null | The From address (the broadcast path authorizes its domain/sender). |
| `html_body` | string \| null | HTML body. |
| `text_body` | string \| null | Plain-text body. |
| `status` | string | Lifecycle state (see below). |
| `total` | integer | Recipient count snapshotted when the send begins. |
| `sent` | integer | Recipients expanded into messages so far. |
| `scheduled_at` | timestamp \| null | Send time for a `scheduled` campaign. |
| `created_at` | timestamp \| null | When the campaign row was created. |
| `completed_at` | timestamp \| null | When expansion finished (`sent` or `failed`). |

## Status lifecycle

A campaign starts as a `draft`, an armed `scheduled` send, or goes
straight to `sending`. From there it advances to a terminal state.

| Status | Meaning | How it is reached | Editable? |
|---|---|---|---|
| `draft` | Composed, saved, not sending. | Create without `send_now` and without `scheduled_at`, or clear a schedule. | Yes |
| `scheduled` | Armed to send at `scheduled_at`. | Create or edit with a `scheduled_at`. | Yes |
| `sending` | Expanding into per-recipient messages. | Send now, or the scheduler claims a due campaign. | No |
| `sent` | Expansion finished; `completed_at` set. | Reached from `sending` when the last batch is done. | No |
| `failed` | A fatal error stopped expansion; `completed_at` set. | The subscriber lookup failed, or a scheduled campaign lost its stream. | No |
| `canceled` | Called off before sending. | Cancel a `draft` or `scheduled` campaign. | No |

`draft` and `scheduled` are the two **planned** states. Edit, send-now
and cancel apply only to those; once a campaign is `sending`, `sent`,
`failed`, or `canceled`, those actions return `422 ValidationError`.

The transitions in one line:

```
draft ─┬─▶ scheduled ─▶ sending ─▶ sent
       │        │            └────▶ failed
       └────────┴──▶ sending (send now)
draft / scheduled ─▶ canceled
```

## Create and edit a campaign

Everything below uses the Server API key and base URL from the
[quickstart](quickstart.md):

```bash
export SERVER_KEY=…              # a server API credential
export API=http://localhost:5000
```

### Create

`POST /api/v2/server/campaigns` creates a server-level campaign. `stream`
(a broadcast stream permalink) and `from` are required. The initial
status follows the request: `send_now` wins, then a `scheduled_at`, else
a `draft`.

```bash
# Save a draft (compose now, decide when to send later)
curl -s -X POST "$API/api/v2/server/campaigns" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{
    "stream": "newsletter",
    "name": "July product update",
    "from": "news@acme.example",
    "subject": "What shipped in July",
    "html_body": "<h1>July</h1><p>Here is what is new.</p>",
    "text_body": "July: here is what is new."
  }'
```

The response is `201` with `{ "campaign": { … } }`, status `draft`.

### Edit

`PATCH /api/v2/server/campaigns/{id}` edits a `draft` or `scheduled`
campaign. Send only the fields you want to change. `name`, `subject`,
`html_body` and `text_body` accept `null` to clear them.

```bash
curl -s -X PATCH "$API/api/v2/server/campaigns/42" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"subject": "What shipped in July (revised)"}'
```

### List and fetch

```bash
# every campaign of the server, newest first, each with its stream
curl -s "$API/api/v2/server/campaigns" -H "X-Server-API-Key: $SERVER_KEY"

# one campaign plus its analytics (see Analytics below)
curl -s "$API/api/v2/server/campaigns/42" -H "X-Server-API-Key: $SERVER_KEY"
```

The dashboard exposes the same surface under **Server → Campaigns**: a
list with each campaign's audience, status, schedule and progress; a
compose form that offers **Send now**, **Schedule** and **Save as
draft**; and a detail page with the analytics tiles and status-dependent
actions.

## Scheduling

A campaign can send immediately or at a chosen time.

**Immediate send.** Two paths flip a campaign straight to `sending`:

```bash
# Create and send at once
curl -s -X POST "$API/api/v2/server/campaigns" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"stream": "newsletter", "from": "news@acme.example",
       "subject": "Live now", "html_body": "<p>Hello subscribers.</p>",
       "send_now": true}'

# Or send an existing draft/scheduled campaign now
curl -s -X POST "$API/api/v2/server/campaigns/42/send" \
  -H "X-Server-API-Key: $SERVER_KEY"
```

Both re-snapshot `total` from the current subscriber count, set the status
to `sending`, and start the async expansion (below).

**Scheduled send.** Give the campaign a `scheduled_at` (an RFC 3339
timestamp) and it enters `scheduled`. Setting a time on a draft moves it
to `scheduled`; clearing the time (`"scheduled_at": null`) drops it back
to `draft`.

```bash
# Schedule at create time
curl -s -X POST "$API/api/v2/server/campaigns" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"stream": "newsletter", "from": "news@acme.example",
       "subject": "Tuesday digest", "html_body": "<p>…</p>",
       "scheduled_at": "2026-07-21T09:00:00Z"}'

# Or arm an existing draft
curl -s -X PATCH "$API/api/v2/server/campaigns/42" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"scheduled_at": "2026-07-21T09:00:00Z"}'
```

An **in-process scheduler** turns due schedules into sends. It runs inside
the `web-server` process and wakes every 30 seconds. On each pass it lists
the servers and, per tenant, atomically claims the `scheduled` campaigns
whose `scheduled_at` has passed, flipping each to `sending` in the same
step so two passes never double-send. It then runs the expansion for each
claimed campaign. A per-server or per-campaign error is logged and the
pass continues, so one bad campaign never stalls the rest. If a scheduled
campaign's stream has gone missing by the time it fires, the campaign is
marked `failed`.

## Async expansion

When a campaign starts sending (send-now, or the scheduler claiming it),
CamelMailer **expands** it into individual messages. The expansion runs in
the background and returns the campaign to the caller right away, so a
create-and-send responds in milliseconds while delivery proceeds behind
it.

The expansion:

1. lists the stream's currently `subscribed` addresses,
2. walks them in batches of 200, sending each through the shared broadcast
   send path with the campaign's From, subject and bodies, on the
   campaign's stream,
3. tags each stored message with the campaign's `campaign_id`,
4. advances the campaign's `sent` counter after every batch, so a poller
   watching the campaign sees it move,
5. marks the campaign `sent` and stamps `completed_at` when the last batch
   is done.

Two counters track progress:

- **`total`** is the audience size snapshotted when the send began (the
  subscriber count at that moment).
- **`sent`** is how many recipients have been expanded into messages so
  far, climbing toward `total`.

Expansion is resilient. A single recipient whose send is rejected (for
example a [suppression](suppressions.md) on that address) is logged and
skipped, and expansion continues. Only a fatal error, the subscriber
lookup itself failing, marks the whole campaign `failed`.

## Analytics

`GET /api/v2/server/campaigns/{id}` returns the campaign plus a `stats`
object aggregated over the messages the campaign produced (attributed by
`campaign_id`) and their tracking data:

```bash
curl -s "$API/api/v2/server/campaigns/42" -H "X-Server-API-Key: $SERVER_KEY"
```

```json
{
  "campaign": { "id": 42, "status": "sent", "total": 1200, "sent": 1200, "…": "…" },
  "stats": {
    "total": 1200,
    "sent": 1200,
    "delivered": 1187,
    "failed": 13,
    "opened": 640,
    "clicked": 219,
    "unsubscribed": 4
  }
}
```

| Stat | What it counts |
|---|---|
| `total` | Audience snapshot from the campaign row. |
| `sent` | Recipients expanded into messages so far. |
| `delivered` | Attributed messages with status `Sent` that are not held. |
| `failed` | Attributed messages that bounced or hard-failed. |
| `opened` | Distinct attributed messages with at least one open. |
| `clicked` | Distinct attributed messages with at least one link click. |
| `unsubscribed` | Stream-scoped `unsubscribe`/`complaint` suppressions created at or after the campaign's `created_at`. |

`opened` and `clicked` come from CamelMailer's open and click tracking, so
they populate only for streams and messages where tracking is enabled. See
[Tracking](tracking.md) for how opens and clicks are recorded, and
[Suppressions](suppressions.md) for how unsubscribes and complaints feed
the `unsubscribed` figure. Because every expanded message carries the
campaign's tag, you can also inspect the individual messages through the
normal message endpoints, and campaign volume shows up in the server-wide
aggregate at `GET /api/v2/server/stats`.

## Cancel a campaign

`POST /api/v2/server/campaigns/{id}/cancel` calls off a `draft` or
`scheduled` campaign and sets its status to `canceled`:

```bash
curl -s -X POST "$API/api/v2/server/campaigns/42/cancel" \
  -H "X-Server-API-Key: $SERVER_KEY"
```

Cancel is a planned-state action. A campaign that is already `sending`,
`sent`, `failed`, or `canceled` returns `422 ValidationError`. To keep a
scheduled campaign from firing, cancel it (or clear its `scheduled_at` to
return it to a draft) before its time arrives.

## A note on local development

The scheduler runs **only in the `web-server` process**. If you boot the
full stack (`docker compose up`), the web server carries it and scheduled
campaigns fire on their own. If you run pieces by hand, a scheduled
campaign stays `scheduled` until a `web-server` is up to claim it.

Expansion queues messages; it does not deliver them. Each expanded message
goes through the same send path as any other broadcast, which enqueues it
for the delivery worker. Without a running `worker`, the expanded messages
pile up in the queue and the campaign still reaches `sent` (its `sent`
counter reflects messages **enqueued**, not messages **delivered**), but
nothing leaves the building until a worker drains the queue. For an
end-to-end local test, run the worker alongside the web server:

```bash
./target/release/camelmailer web-server &   # HTTP API + campaign scheduler
./target/release/camelmailer worker &       # delivers the expanded messages
```

## Quick reference

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/v2/server/campaigns` | List every campaign of the server. |
| `POST` | `/api/v2/server/campaigns` | Create a campaign (`draft`, `scheduled`, or `send_now`). |
| `GET` | `/api/v2/server/campaigns/{id}` | Fetch one campaign with its `stats`. |
| `PATCH` | `/api/v2/server/campaigns/{id}` | Edit a `draft`/`scheduled` campaign. |
| `POST` | `/api/v2/server/campaigns/{id}/send` | Send a `draft`/`scheduled` campaign now. |
| `POST` | `/api/v2/server/campaigns/{id}/cancel` | Cancel a `draft`/`scheduled` campaign. |

Related: [Broadcast streams](broadcast.md) ·
[Message streams](streams.md) · [Tracking](tracking.md) ·
[Suppressions](suppressions.md)
