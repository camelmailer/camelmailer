# Open and click tracking

CamelMailer can measure engagement on the mail it sends: whether a
message was opened, and which of its links a recipient followed. Two
techniques do the work, and both are applied by the delivery worker at
send time so the numbers roll up into message detail, per-server
[statistics](sending.md) and [campaign](campaigns.md) analytics:

1. **Open tracking** appends an invisible 1×1 GIF (a *pixel*) before
   `</body>`. When the recipient's mail client loads that image, it hits
   a CamelMailer endpoint and an open is recorded.
2. **Click tracking** rewrites every `http(s)` link in the HTML body to
   point at CamelMailer. The endpoint records the click and then
   302-redirects the recipient to the original URL, which is preserved
   exactly.

Tracking touches the **HTML part only**. A text-only message keeps its
body verbatim and produces no open or click events.

## What each technique does

| | Open tracking | Click tracking |
|---|---|---|
| Mechanism | 1×1 transparent GIF injected before `</body>` | `href` values rewritten to a redirect URL |
| Endpoint | `GET /track/o/{token}.gif` | `GET /track/c/{token}` |
| Response | the pixel, with `Cache-Control: no-store` | `302 Found` to the original URL |
| Recorded in | the `loads` table | the `link_clicks` table |
| Fires when | the client loads remote images | the recipient clicks the link |
| Data captured | IP address, User-Agent, timestamp | IP address, User-Agent, clicked URL, timestamp |

## Turning tracking on

Tracking is configured **per mail server**. A server carries two
booleans, `track_opens` and `track_clicks`, the defaults for mail it
sends over the HTTP API. Read them on the server record and set them
through the management API:

```bash
# inspect current settings
curl -s "$API/api/v2/admin/organizations/acme/servers/production" \
  -H "X-Admin-API-Key: $ADMIN_KEY"
#   … "track_opens": true, "track_clicks": true …

# change them
curl -s -X PATCH "$API/api/v2/admin/organizations/acme/servers/production" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"track_opens": false, "track_clicks": false}'
```

The dashboard exposes the same two toggles on the server settings page.
There is one granularity here: the mail server. Individual messages and
[streams](streams.md) inherit the server's setting.

> **Honest note on the current build.** The two flags are stored and
> editable, and they surface everywhere a server is shown. The delivery
> worker's tracking pass, however, rewrites the HTML of **every**
> outgoing message that has an HTML part, without yet reading these
> per-server flags. Treat the toggles as the intended control surface and
> verify behaviour against your own installation before relying on them
> to suppress tracking. To be certain a message carries no tracking
> today, send it as text only, or run the server in privacy mode (below).

## The tracking domain

Both public endpoints are served by the CamelMailer **web** process (the
same one that answers the API, port 5000 in the [quickstart](quickstart.md)
stack). Recipients reach them through a dedicated hostname, the
**tracking domain**, set by `dns.track_domain`:

```yaml
dns:
  track_domain: track.example.com
```

Publish it as a CNAME to the web server (see the DNS table in
[Configuration](configuration.md)). The worker builds every tracking URL
as `<web_protocol>://<track_domain>`, for example
`https://track.example.com`, so the domain has to be publicly resolvable
and reachable over the web protocol for the pixel to load and the
redirect to work.

### Per-server track domains

The installation-wide `dns.track_domain` is the default. A mail server
can carry its **own** track domains on top, so tracking links live under
the sender's brand (`track.acme.com`) instead of the platform's. When a
server has at least one **verified** track domain, the worker and the
List-Unsubscribe headers use it (the first verified one, by id) instead
of the global default. Servers without one keep using
`dns.track_domain` — nothing changes for them.

Manage them under
`/api/v2/admin/organizations/{org}/servers/{server}/track_domains`:

```bash
# add — the response carries the CNAME record to publish
curl -s -X POST "$API/…/track_domains" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"name": "track.acme.com"}'

# publish:  track.acme.com.  CNAME  mail.example.com.

# verify — resolves the CNAME live; machine keys may pass {"force": true}
curl -s -X POST "$API/…/track_domains/{id}/verify" -H "X-Admin-API-Key: $ADMIN_KEY"
```

A track domain must CNAME to this installation (the web hostname or the
installation-wide track domain): the public `/track/*` endpoints resolve
tokens regardless of the host they were reached on, so the CNAME is all
it takes. Verification checks exactly that record and names it in the
error until it is in place; `{"force": true}` (machine key only) skips
the check, mirroring sending-domain verification.

## The public endpoints

Both endpoints are unauthenticated: the incoming request carries only an
opaque token. They sit behind the load balancer or reverse proxy that
terminates TLS, so the client IP is read from the first entry of the
`X-Forwarded-For` header and the client from `User-Agent`.

| Endpoint | On a match | On anything else |
|---|---|---|
| `GET /track/c/{token}` | records a click, returns `302 Found` with `Location:` set to the stored original URL | `404 Not Found` |
| `GET /track/o/{token}.gif` | records an open, returns the 1×1 GIF | returns the same GIF |

The open endpoint always answers with the pixel and never signals
whether the token was valid, so a scraped or expired pixel URL reveals
nothing. The click endpoint answers `404` for an unknown token or a
token that is not a click token.

A third public route, `GET`/`POST /track/u/{token}`, handles one-click
unsubscribe for broadcast mail. It is documented with
[broadcast streams and campaigns](campaigns.md).

## How tokens are created and resolved

Tokens are **pre-registered by the worker at delivery time**, in the same
pass that rewrites the body and before DKIM signing (so the signature
covers the final, rewritten message). For each outgoing HTML message the
worker:

1. Finds every `href="http(s)://…"` in the HTML body.
2. For each such link, stores a row in the `links` table, then generates
   a 24-character click token and inserts it into `tracking_tokens` with
   `kind = 'click'`, the `server_id`, `message_id`, `link_id` and the
   original `target_url`.
3. Replaces the link with `<track_domain>/track/c/<token>`.
4. Generates one open token (`kind = 'open'`, `server_id`, `message_id`,
   no link or URL) and injects the pixel `<track_domain>/track/o/<token>.gif`
   before `</body>`.

`tracking_tokens` is a **cross-tenant lookup table**: because the public
endpoints are unauthenticated and carry only a token, resolution is a
single `SELECT … WHERE token = $1` that yields which tenant, message and
(for clicks) URL the token belongs to. The recorded event then lands in
the tenant-scoped `loads` or `link_clicks` table, written inside that
server's row-level-security context. Isolation holds even though the
lookup itself spans tenants.

## What a rewrite looks like

Take this HTML message body:

```html
<html>
  <body>
    <p>Please confirm your address:
       <a href="https://acme.example/confirm?u=42">Confirm</a></p>
    <p>Questions? <a href="mailto:help@acme.example">Email us</a></p>
  </body>
</html>
```

With `track_domain = track.example.com`, the worker delivers this
instead:

```html
<html>
  <body>
    <p>Please confirm your address:
       <a href="https://track.example.com/track/c/Xk3f9Qm2Lp7Rt1Vb8Nd4Zc0">Confirm</a></p>
    <p>Questions? <a href="mailto:help@acme.example">Email us</a></p>
    <img src="https://track.example.com/track/o/9pQ2Ls6Wk1Fj7Ht3Vb8Nd4Zc0.gif"
         alt="" width="1" height="1" style="display:none"/>
  </body>
</html>
```

Points worth noting:

- Only the `http(s)` link was rewritten. The `mailto:` link (and any
  other non-web scheme) is left untouched.
- The original `https://acme.example/confirm?u=42` is kept in the click
  token's `target_url`. When the recipient follows the rewritten link,
  `GET /track/c/Xk3f…` records the click and 302-redirects the browser
  straight to `https://acme.example/confirm?u=42`, so the recipient
  arrives at the intended page.
- The pixel is `display:none` and 1×1, invisible in a rendered message.

## Where opens and clicks show up

Every event is readable through the Server API (`X-Server-API-Key`) and
in the dashboard.

**Per message.** Two endpoints list the raw events, newest first, each
with IP address, User-Agent and timestamp (clicks also carry the URL):

```bash
curl -s "$API/api/v2/server/messages/8/opens"  -H "X-Server-API-Key: $SERVER_KEY"
curl -s "$API/api/v2/server/messages/8/clicks" -H "X-Server-API-Key: $SERVER_KEY"
```

The message detail page renders these as a lifecycle timeline,
`Sent → Delivered → Opened → Clicked`, using the first open and first
click.

**Aggregated.** `GET /api/v2/server/stats` returns `opens`, `clicks`,
`unique_opens` and `unique_clicks` over the window, and the same counters
feed the Statistics view.

**Per campaign.** A [campaign](campaigns.md) rolls its opens and clicks
up over exactly the messages it produced, joining `loads` and
`link_clicks` back to the attributed messages.

A message shared through a public
[share link](quickstart.md) carries its opens and clicks along with the
decoded bodies, so support can triage engagement without an account.

## Privacy and disabling tracking

Open and click tracking record the recipient's IP address and
User-Agent. For an EU-focused deployment, weigh that against your legal
basis and disclosures before enabling it. Several controls help:

- **Disable per server.** Set `track_opens` and `track_clicks` to
  `false` on the mail server (see the honest note above about the current
  worker behaviour).
- **Send text only.** A message with a text body and no HTML part is
  never rewritten and produces no events.
- **Privacy mode.** A server in `privacy_mode` does not retain message
  content: the raw MIME endpoint answers `404 NotAvailable` and the
  dashboard hides message bodies.
- The open pixel is served with `Cache-Control: no-store` and the
  endpoint keeps token validity opaque, so tracking URLs cannot be probed
  for whether a given message exists.

## Tracking in local development

Tracking events depend on two things that a laptop stack usually lacks:

- **The delivery worker.** The rewrite and token registration happen only
  when the worker actually processes and sends a queued message. If the
  worker is not running, or the message never leaves the queue, no
  tracking is applied.
- **A reachable tracking domain.** An open is recorded when the
  recipient's client loads `https://<track_domain>/track/o/…`, and a
  click when their browser hits `https://<track_domain>/track/c/…`. On a
  local stack the `track_domain` is typically not publicly resolvable,
  and test messages rarely reach a real inbox, so those requests never
  arrive.

The practical consequence: in a local development stack you will see the
rewritten HTML in the delivered message, but the `opens`/`clicks`
endpoints stay empty until real recipients load the pixel and follow the
links against a reachable tracking domain.

## See also

- [Sending email](sending.md): the send API and the delivery pipeline
  that applies tracking.
- [Campaigns](campaigns.md): per-broadcast open/click analytics and
  one-click unsubscribe.
- [Message streams](streams.md): how transactional and broadcast mail is
  organised.
- [Configuration](configuration.md): the `dns.track_domain` record and
  the rest of the DNS setup.
