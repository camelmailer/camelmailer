# Webhooks

A webhook is an HTTP callback Camelmailer sends to your application when
something happens to a message: it was accepted by the recipient's mail
server, it was deferred, it failed permanently, it was held before
sending, a later DSN reported that it bounced, the recipient opened it or
clicked a link, or the recipient's mailbox provider reported a spam
complaint. You register a URL per
mail server, choose which events it
receives, and Camelmailer POSTs a small JSON body to that URL every time
a matching event fires. Each request carries an RSA signature so your
receiver can confirm the payload really came from your installation.

Webhooks report on the delivery lifecycle of your outgoing mail. For
inbound mail that arrives at your server, use an
[inbound route](inbound.md) with an HTTP endpoint target instead. How
opens and clicks are recorded is described in [Tracking](tracking.md).

## Event types

Camelmailer fires eight events about the fate of outgoing messages. These
names are the single source of truth (`WEBHOOK_EVENTS` in
`camelmailer-core`); the API rejects any other value at registration
time.

| Event | Fires when | Delivery status recorded |
|---|---|---|
| `MessageSent` | The recipient's mail server accepted the message. | `Sent` |
| `MessageDelayed` | A delivery attempt got a temporary (4xx) failure and the message is scheduled for another try. | `SoftFail` |
| `MessageDeliveryFailed` | Delivery failed for good: a hard (5xx) rejection, or the retry attempts were exhausted. | `HardFail` |
| `MessageHeld` | An outgoing message was held before sending. Today this fires when the recipient is on the server's [suppression list](suppressions.md). | `Held` |
| `MessageBounced` | A DSN received at the return path reported `Action: failed` and was correlated to the original outgoing message. Delay and successful-delivery DSNs do not fire this event. | `Bounced` on the original; `Processed` on the DSN |
| `MessageLoaded` | A recipient loaded the open-tracking pixel. Fires on every load. | none (a `loads` row) |
| `MessageLinkClicked` | A recipient followed a tracked link. Fires on every click. | none (a `link_clicks` row) |
| `MessageComplained` | A feedback-loop (ARF) abuse report arrived for an outgoing message. See [Feedback loops](suppressions.md#feedback-loops-fbl--arf). | `Processed` on the report |

A message can produce several events over its life. A message that is
deferred twice and then delivered produces two `MessageDelayed` events
followed by one `MessageSent`.

## Payload shape

Every delivery has the same envelope. The top level names the event and
carries a per-delivery `uuid` and a Unix `timestamp` (seconds); the
event-specific data sits under `payload`.

```json
{
  "event": "MessageSent",
  "timestamp": 1720000000,
  "uuid": "6f1c2b7e-2a4d-4c9e-9f3a-6d8b0e1f2a3c",
  "payload": {
    "message": {
      "id": 1234,
      "token": "AbCdEf123456",
      "rcpt_to": "recipient@example.com",
      "mail_from": "sender@yourdomain.com",
      "scope": "outgoing",
      "bounce": false
    },
    "details": "message accepted by the remote server"
  }
}
```

The envelope fields:

| Field | Type | Meaning |
|---|---|---|
| `event` | string | One of the eight event names above. |
| `timestamp` | integer | When the event was enqueued, Unix seconds. |
| `uuid` | string | Unique per delivery. Also sent as the `X-CamelMailer-UUID` header, and stable across retries of the same event, so it doubles as an idempotency key. |
| `payload.message.id` | integer | The message id (matches the id in the messages API and dashboard). |
| `payload.message.token` | string | The message token used in tracking and observability URLs. |
| `payload.message.rcpt_to` | string | The envelope recipient. |
| `payload.message.mail_from` | string | The submitted envelope sender. SMTP delivery uses the configured shared return path when available. |
| `payload.message.scope` | string | `outgoing` for the message-delivery events above. |
| `payload.message.bounce` | boolean | Whether the message is a bounce. |
| `payload.details` | string | A short human-readable reason. For `MessageSent` it is the remote server's acceptance line; for the failure and delay events it is the SMTP response; for `MessageHeld` it explains the hold. |

The first four events use the common `message` and `details` payload above.
The other four use Postal's message hash instead (`id`, `token`,
`direction`, `message_id`, `to`, `from`, `subject`, `timestamp`,
`spam_status`, `tag`). `MessageBounced` carries both correlated messages:

Camelmailer inspects the inbound DSN before building this payload, so the
`bounce.spam_status` value is the stored inspection verdict. Reprocessing an
already correlated DSN does not emit another `MessageBounced` event. For
Postal compatibility, `message_id` omits surrounding angle brackets and
`subject` is MIME-decoded, limited to 200 characters, and represented by an
empty string when the message has no Subject header.

```json
{
  "event": "MessageBounced",
  "timestamp": 1720000060,
  "uuid": "5d42fbea-41d8-4701-b5b5-d572166c51fb",
  "payload": {
    "original_message": {
      "id": 1234,
      "token": "abc123message",
      "direction": "outgoing",
      "message_id": "original@example.com",
      "to": "recipient@example.com",
      "from": "sender@yourdomain.com",
      "subject": "Example message",
      "timestamp": 1720000000.123,
      "spam_status": "NotChecked",
      "tag": null
    },
    "bounce": {
      "id": 1235,
      "token": "def456bounce",
      "direction": "incoming",
      "message_id": "bounce@mx.example.com",
      "to": "server1@rp.example.com",
      "from": "mailer-daemon@mx.example.com",
      "subject": "Delivery Status Notification (Failure)",
      "timestamp": 1720000060.123,
      "spam_status": "NotChecked",
      "tag": null
    }
  }
}
```

`MessageLoaded` and `MessageLinkClicked` carry the opened message, the
client's IP address (first `X-Forwarded-For` hop) and User-Agent (capped
at 255 characters), and the event time as Unix seconds with milliseconds.
A click adds the original `url`:

```json
{
  "event": "MessageLinkClicked",
  "timestamp": 1720000300,
  "uuid": "0c3b9a52-7f0e-4b8f-9a55-1c2d3e4f5a6b",
  "payload": {
    "message": {
      "id": 1234,
      "token": "abc123message",
      "direction": "outgoing",
      "message_id": "original@example.com",
      "to": "recipient@example.com",
      "from": "sender@yourdomain.com",
      "subject": "Example message",
      "timestamp": 1720000000.123,
      "spam_status": "NotChecked",
      "tag": null
    },
    "url": "https://example.com/offer",
    "ip_address": "203.0.113.10",
    "user_agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)",
    "timestamp": 1720000300.456
  }
}
```

`MessageLoaded` has the same shape without `url`. Neither event fires
when the message itself has been pruned by retention.

`MessageComplained` names the complaining `address`, the report's
`feedback_type` (currently always `abuse`) and, when the report carries
one, the reporting system's `user_agent`. `message` is the matched
outgoing message, or `null` when the report only identifies the
recipient. See [Feedback loops](suppressions.md#feedback-loops-fbl--arf)
for how a report is matched.

```json
{
  "event": "MessageComplained",
  "timestamp": 1720086400,
  "uuid": "a7e1f0d2-3c4b-4d5e-8f90-123456789abc",
  "payload": {
    "message": {
      "id": 1234,
      "token": "abc123message",
      "direction": "outgoing",
      "message_id": "original@example.com",
      "to": "recipient@example.com",
      "from": "sender@yourdomain.com",
      "subject": "Example message",
      "timestamp": 1720000000.123,
      "spam_status": "NotChecked",
      "tag": null
    },
    "address": "recipient@example.com",
    "feedback_type": "abuse",
    "user_agent": "Example-FBL/1.0",
    "timestamp": 1720086400.789
  }
}
```

Test deliveries add one extra top-level field, `"test": true` (see
[Testing a webhook](#testing-a-webhook)); real deliveries never carry it.

## Signing

When a webhook has signing enabled (the default), Camelmailer signs the
exact request body and sends the signature in a header.

| Property | Value |
|---|---|
| Header | `X-CamelMailer-Signature` |
| Algorithm | RSA PKCS#1 v1.5 over a SHA-256 digest |
| Encoding | Standard base64 of the raw signature bytes |
| Signed content | The complete request body, byte for byte |
| Key | The installation signing key (`camelmailer.signing_key_path`) |

The signing key is the installation's RSA key, the same key Camelmailer
uses for DKIM. Its public half is therefore the value published as your
DKIM `p=` DNS record. To get a PEM copy for your receiver, export the
public half of the signing key once:

```bash
openssl rsa -in /path/to/signing.key -pubout -out camelmailer-webhooks.pub
```

To verify a request: take the raw request body exactly as received (do
not re-serialize the JSON, since any reformatting changes the bytes and
breaks the signature), base64-decode the `X-CamelMailer-Signature`
header, and check it against the body with the public key.

Node.js:

```js
const crypto = require("crypto");

function verify(rawBody, signatureHeader, publicKeyPem) {
  const verifier = crypto.createVerify("RSA-SHA256");
  verifier.update(rawBody);          // the exact bytes received
  verifier.end();
  return verifier.verify(publicKeyPem, signatureHeader, "base64");
}
```

Python (`cryptography`):

```python
import base64
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding
from cryptography.exceptions import InvalidSignature

def verify(raw_body: bytes, signature_header: str, public_key_pem: bytes) -> bool:
    public_key = serialization.load_pem_public_key(public_key_pem)
    try:
        public_key.verify(
            base64.b64decode(signature_header),
            raw_body,                  # the exact bytes received
            padding.PKCS1v15(),
            hashes.SHA256(),
        )
        return True
    except InvalidSignature:
        return False
```

If the installation has no signing key on disk, signing is disabled: the
worker logs a warning at startup and deliveries go out without the
signature header. A webhook with `sign` set to false also skips the
header. Treat a missing signature as untrusted when you rely on signing.

## Delivery semantics

Each event is fanned out to every enabled webhook that subscribes to it,
and each delivery becomes a row in a per-server queue that the worker
drains.

For `MessageBounced`, Camelmailer inserts every subscribed queue row in the
same database transaction that links the DSN and marks the original message
`Bounced`. If any request insert fails, the full correlation rolls back and a
later message-queue attempt can retry it. A repeated attempt after that
transaction commits does not queue the event twice. When a DSN beats an
in-flight outbound result to the message row, the later result cannot enqueue
`MessageSent`, `MessageDelayed`, `MessageDeliveryFailed`, or `MessageHeld`.
Each outbound delivery-state transition commits its delivery row, queue
completion or retry, and subscribed webhook requests together. This ordering
also prevents a retry or event from escaping after correlation has recorded
the newer bounce. Subscriber rows stay locked while Camelmailer inserts their
requests. Deleting a webhook during fan-out waits for this transaction to
commit, then removes the newly queued request through the existing cascade
without rolling back the message state or queue action.

`MessageLoaded` and `MessageLinkClicked` requests commit with the recorded
open or click, and `MessageComplained` requests with the complaint
suppression, so neither side can exist without the other.

- **Queue.** Deliveries are stored in a `webhook_requests` table and
  picked up one at a time with `FOR UPDATE SKIP LOCKED`, so several
  workers can run in parallel without delivering the same request twice.
- **Success.** Any 2xx response completes the delivery and removes it
  from the queue.
- **Retries and backoff.** A non-2xx response, a connection error, or a
  timeout schedules a retry with exponential backoff of `2^attempts`
  minutes, capped at 24 hours. The schedule runs 1, 2, 4, 8, 16, 32, 64,
  128, and 256 minutes.
- **Giving up.** A delivery is attempted at most 10 times. After the
  tenth failed attempt the worker gives up, logs a warning, and drops
  the request from the queue.
- **Audit log.** Every attempt is recorded in a tenant-scoped
  `webhook_request_log` with the event, URL, attempt number, HTTP status
  code, success flag, and the first 2 KB of the response body. Custom
  header values are treated as secrets and are kept out of the log.

Because retries can span hours, design your receiver to be idempotent:
respond 2xx as soon as you have durably accepted the event, and use the
`uuid` to detect a repeat of one you already processed.

### Request headers

Every delivery carries these headers:

| Header | Value |
|---|---|
| `Content-Type` | `application/json` |
| `X-CamelMailer-Event` | The event name, matching `event` in the body. |
| `X-CamelMailer-UUID` | The delivery uuid, matching `uuid` in the body. |
| `X-CamelMailer-Signature` | The base64 RSA signature (present only when signing is enabled and a key exists). |

These three header names keep the `CamelMailer` spelling the product
shipped with, while the product itself is written Camelmailer. They are
frozen so that a receiver matching on them keeps working. Header field
names are case-insensitive (RFC 9110), so a client library that
normalizes them, as Go and Node do, may hand you a different casing.

Any custom `headers` you set on the webhook are added first, and the
platform `X-CamelMailer-*` headers are applied last, so a custom header
can never override a platform one. Custom headers are the place for a
shared secret such as `Authorization: Bearer ...` if you prefer a bearer
token to signature verification.

## Managing webhooks

Webhooks are a server resource under the admin API. Authenticate with an
`X-Admin-API-Key` or a user session (RBAC: member and above may manage
webhooks). See [Accounts, RBAC & SSO](authentication.md).

```text
GET    /api/v2/admin/organizations/{org}/servers/{server}/webhooks
POST   /api/v2/admin/organizations/{org}/servers/{server}/webhooks
GET    /api/v2/admin/organizations/{org}/servers/{server}/webhooks/{id}
PATCH  /api/v2/admin/organizations/{org}/servers/{server}/webhooks/{id}
DELETE /api/v2/admin/organizations/{org}/servers/{server}/webhooks/{id}
POST   /api/v2/admin/organizations/{org}/servers/{server}/webhooks/{id}/enable
POST   /api/v2/admin/organizations/{org}/servers/{server}/webhooks/{id}/disable
POST   /api/v2/admin/organizations/{org}/servers/{server}/webhooks/{id}/test
```

A webhook has these fields:

| Field | Type | Notes |
|---|---|---|
| `name` | string | Required on create. |
| `url` | string | Required, must start with `http://` or `https://`. |
| `events` | string[] | Subscribed event names. An empty or omitted list subscribes to every event. An unknown name is a `ValidationError` that lists the valid ones. |
| `sign` | boolean | Whether to add the RSA signature. Defaults to `true`. |
| `headers` | object | Extra HTTP headers set on every delivery. Values are secrets and are never logged. |
| `enabled` | boolean | New webhooks start enabled. Toggle with the `enable`/`disable` endpoints or a `PATCH`. |
| `all_events` | boolean | Read-only. `true` when `events` is empty (subscribed to everything). |

### Registering an endpoint

```bash
curl -X POST \
  https://mail-admin.example.com/api/v2/admin/organizations/acme/servers/production/webhooks \
  -H "X-Admin-API-Key: <key>" \
  -H "Content-Type: application/json" \
  -d '{
        "name": "delivery-events",
        "url": "https://app.example.com/hooks/camelmailer",
        "events": ["MessageSent", "MessageDeliveryFailed"],
        "sign": true,
        "headers": { "Authorization": "Bearer s3cr3t" }
      }'
```

The response wraps the created webhook:

```json
{
  "status": "success",
  "data": {
    "webhook": {
      "id": 7,
      "uuid": "…",
      "name": "delivery-events",
      "url": "https://app.example.com/hooks/camelmailer",
      "all_events": false,
      "enabled": true,
      "sign": true,
      "events": ["MessageSent", "MessageDeliveryFailed"],
      "headers": { "Authorization": "Bearer s3cr3t" }
    }
  }
}
```

To receive every event, send an empty `events` list (or omit it). To
change the subscription later, `PATCH` the webhook with a new `events`
array.

### Testing a webhook

`POST .../webhooks/{id}/test` with `{ "event": "MessageSent" }`
synchronously delivers one sample payload of that event to the webhook
URL, with the same custom headers and RSA signature the worker would
send. The sample body carries `"test": true` so your receiver can tell
it apart from a real event. The response reports the outcome and nothing
is queued, retried, or written to the audit log:

```json
{ "delivered": true, "status_code": 200, "duration_ms": 84 }
```

The call uses a 10-second timeout; a transport failure returns
`delivered: false` with an `error` string.

## In the dashboard

Each server has a **Webhooks** tab (under **Server** in the dashboard).
It lists the server's webhooks and lets a member and above add one, set
the URL, pick events, toggle signing, add custom headers, and enable or
disable it. The detail view shows the example payload for each subscribed
event and offers the same test-send as the API, so you can confirm your
receiver works before real traffic flows.

## Honest gaps

- `MessageComplained` fires for ARF abuse reports only. The manual
  complaint endpoint (`POST …/subscribers/{address}/complaint`) records the
  suppression without a webhook, since it has no message to report.
- `MessageHeld` currently fires for the suppression-list hold on outgoing
  mail. Inbound messages held by spam or virus inspection are stored and
  visible on the message, and do not emit a webhook.
- There is no dedicated endpoint that serves the webhook public key.
  Derive it from the installation signing key (or the DKIM `p=` record),
  which is stable for the installation.
</content>
</invoke>
