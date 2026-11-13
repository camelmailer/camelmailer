# Configuration

Camelmailer reads a single YAML file. Everything has a sensible default —
an empty (or missing) file is a valid configuration; the two most common
deployments need only environment variables.

## Where configuration comes from

| Source | Purpose |
|---|---|
| `CAMELMAILER_CONFIG_FILE_PATH` | Path to the YAML file (`POSTAL_CONFIG_FILE_PATH` also works; default `config/camelmailer/camelmailer.yml`) |
| `DATABASE_URL` | PostgreSQL URL — **takes precedence** over the `postgres:` group |
| `PORT` / `BIND_ADDRESS` | Web-server listen overrides (used by the Docker image) |
| `RUST_LOG` | Log filter (`info`, `debug`, `camelmailer_worker=debug,info`, …) |

A full annotated example lives at
[`config/camelmailer.example.yml`](../config/camelmailer.example.yml).
Existing Postal installations: your `postal.yml` loads unchanged — the
`postal:` group is accepted as an alias for `camelmailer:`.

`$config-file-root` inside path values expands to the directory containing
the config file.

## The groups that matter

### `camelmailer:` — installation identity

```yaml
camelmailer:
  web_hostname: mail.example.com     # public hostname of the HTTP API
  smtp_hostname: mx.example.com      # HELO identity of the SMTP server
  signing_key_path: $config-file-root/signing.key
  # admin_api_key: …                 # global fallback admin key (prefer DB keys)
  # smtp_relays: ["smtp://relay:25"] # deliver via relays instead of direct-to-MX
```

**Relay URLs** express port, TLS mode and credentials. Direct MX delivery
always uses port 25 — that is how the protocol works — so `smtp_relays`
is the way to send when your provider blocks outbound port 25:

| URL | Behaviour |
|---|---|
| `smtp://host:25` | plaintext + opportunistic STARTTLS |
| `smtp://host:587` | submission — STARTTLS **enforced**, no plaintext fallback |
| `smtps://host:465` | implicit TLS from the first byte |
| `smtp://user:pass@host:587` | AUTH PLAIN after the TLS handshake (percent-encode special characters) |

**Certificate verification** differs by target. **Direct-to-MX** delivery
uses opportunistic TLS *without* verifying the remote certificate (like a
normal MTA's `smtp_tls_security_level = may`) and falls back to plaintext if
the handshake fails — a foreign MX's certificate is not issued for your
benefit, so requiring a trust chain would fail against nearly every real MX.
`smtp.openssl_verify_mode` (`peer` = verify against webpki roots, the default;
`none` = accept any certificate) therefore applies to **configured relays
only**, whose identity is known; `smtps://` relays use implicit TLS with
verification by default.

The **signing key** is one RSA private key used for webhook payload
signing and as the DKIM fallback:

```bash
openssl genrsa -out signing.key 2048
```

Every domain created through the API gets its **own RSA-2048 DKIM key**
(generated and stored server-side; the private key never leaves the
installation). The installation signing key remains the DKIM fallback
for domains created before per-domain keys existed — that fallback stays
valid forever. Without a signing key the stack still runs — the worker
logs a warning and skips webhook signing and DKIM for fallback-key
domains; domains with their own key are always signed.

### `postgres:` — storage

```yaml
postgres:
  enabled: true
  host: localhost
  username: camelmailer
  password: secret
  database: camelmailer
  pool_size: 10
```

Or just set `DATABASE_URL`. Without either, the servers fall back to
**non-persistent in-memory storage** (fine for kicking the tires, useless
for production — a warning is logged).

Camelmailer uses one PostgreSQL database for everything; tenant isolation
on message data is enforced *by the database* via row-level security, not
by application code. No per-tenant databases to manage.

### `smtp_server:` — intake

```yaml
smtp_server:
  default_port: 25
  max_message_size: 14        # MB
  tls_enabled: true           # STARTTLS termination
  auth_requires_tls: true     # default; see "Turning TLS on" below
  tls_certificate_path: $config-file-root/smtp.cert
  tls_private_key_path: $config-file-root/smtp.key
  proxy_protocol: false       # enable behind HAProxy/NLB
  listeners:                  # additional ports (default: none)
    - { port: 587, mode: smtp }
    - { port: 465, mode: smtps }
```

The server can listen on several ports at once. `default_port` always
speaks plain SMTP with optional STARTTLS; each `listeners` entry adds a
port in mode `smtp` (same behaviour) or `smtps` (implicit TLS from the
first byte — the classic port 465, requires `tls_enabled` and a
certificate). Ports must be distinct; the session behaves identically on
every listener — on `smtps` it simply starts in the TLS state (AUTH
available immediately, messages marked as received over TLS).

**The certificate is re-read when it changes on disk.** The server stats the
certificate and key before each handshake and rebuilds its TLS acceptor when
a modification time moves, so an ACME renewal reaches a running server
without a restart. A file caught mid-write keeps the previous certificate in
service and is retried on the next handshake.

#### Behind a TCP load balancer (`proxy_protocol`)

With `proxy_protocol: true` every listener expects a PROXY protocol v1 line
(`PROXY TCP4 <client> <proxy> <client-port> <proxy-port>`) as the first thing
on each connection, and takes the client IP from it. Inbound SPF and IP-based
relay credentials then see the real client instead of the load balancer.
Without it, every client looks like the balancer's address.

On `smtp` listeners the line comes before the banner. On `smtps` listeners
the balancer sends it in the clear in front of the client's TLS handshake;
the server reads exactly that line from the raw socket, then starts TLS and
sends the banner over the encrypted stream. A connection without a valid
header is closed, on every listener, so do not expose a `proxy_protocol`
listener to clients that bypass the balancer: they cannot connect, and
nothing lets them claim another address.

The switch applies to all listeners of the process. To serve some clients
directly, run a second `smtp-server` without it.

#### Turning TLS on for an installation that already authenticates

AUTH is advertised, and accepted, only on a TLS-protected session. An
installation with `tls_enabled: false` is the exception: there is no STARTTLS
to offer, so AUTH stays available, on the assumption that TLS terminates
elsewhere or the network is trusted.

That makes the switch sharper than it looks. The moment `tls_enabled` becomes
true, AUTH starts requiring TLS, and any client configured without STARTTLS
fails at that instant. Most client libraries upgrade on their own once
STARTTLS is advertised, so the ones at risk are those with TLS explicitly
disabled.

`auth_requires_tls: false` is the window for that transition:

1. Deploy the certificate with `tls_enabled: true` and
   `auth_requires_tls: false`. STARTTLS is now offered, and clients that
   handle it upgrade by themselves.
2. Watch the log. Every AUTH accepted on an unencrypted session is a warn
   line naming the credential, its server and the client address:
   `SMTP AUTH accepted on an unencrypted session`.
3. Move the clients the log names.
4. Remove `auth_requires_tls` (or set it to `true`) once the log stays quiet.

The server logs a warning at startup for as long as the window is open, so an
installation cannot forget it is in this state.

### `dns:` — the records you publish

For deliverability you publish, per installation:

| Record | Config key | Example |
|---|---|---|
| MX for inbound | `dns.mx_records` | `mx.example.com` |
| SPF include | `dns.spf_include` | `v=spf1 include:spf.example.com ~all` on sender domains |
| DKIM selector | `dns.dkim_identifier` | `camelmailer._domainkey.<domain>` TXT with the domain key's public part (installation key for pre-existing domains) |
| Return-path | `dns.return_path_domain` | MX to Camelmailer plus an SPF TXT record authorizing its outbound senders |
| Click/open tracking | `dns.track_domain` | CNAME → the web server |

`dns.return_path_envelope` defaults to `false` in 0.7.x. Setting a
return-path domain alone configures intake and does not change outbound
`MAIL FROM`. Publish the return-path MX and SPF records, then explicitly set
`return_path_envelope: true` to enable envelope rewriting. Empty, invalid,
or reserved example domains still preserve the submitted sender and produce
a startup warning when rewriting is enabled.

You don't have to assemble these by hand for sending domains:
`GET /api/v2/admin/…/domains/{name}` (and the dashboard's *DNS records*
dialog) returns the exact `verification_record`, `spf_record` and
`dkim_record` to publish. Domain ownership is proven via DNS — publish
the TXT record `_camelmailer-challenge.<domain>` with the value
`camelmailer-verification=<token>` and call
`POST …/domains/{name}/verify`. Operators can skip the check with
`{"force": true}` using the `X-Admin-API-Key` machine key.

The return-path domain is installation-wide, so the domain API does not
publish its records. Replace the sample `rp.example.com`, route the real
domain's MX to Camelmailer, and add an SPF TXT record for every IP or relay
used by the worker. Until the value is a usable non-placeholder domain, the
worker preserves each message's submitted envelope sender and cannot
correlate returned DSNs through the shared return path. The SMTP intake still
accepts return-path mail addressed to a syntactically valid configured
placeholder domain, since the reserved-name check applies only to the outbound
send-side fallback. Empty or malformed values cannot match an inbound domain.
Camelmailer canonicalizes a configured value to lowercase ASCII, including
IDNA conversion and removal of a trailing root dot, and uses that same form for
sending and SMTP intake.

### `rspamd:` / `clamav:` — inbound inspection (optional)

Both disabled by default; point them at running rspamd/ClamAV instances to
spam-score and virus-scan inbound mail. Failing messages are held.

### `auth:` / `app_mail:` / `oidc:` / `saml:` / `scim:` / `web_server.cors_origins` — accounts, SSO, provisioning, browsers

User accounts (sessions, 2FA, lockout), passkeys (WebAuthn), organization
RBAC, invitations, platform email delivery, OIDC single sign-on and CORS
are documented in **[authentication.md](authentication.md)**. Quick anchors:
User accounts (sessions, 2FA, lockout), organization RBAC, invitations,
platform email delivery, OIDC and SAML single sign-on, SCIM
provisioning and CORS are documented in
**[authentication.md](authentication.md)**. Quick anchors:

```yaml
auth:
  session_timeout_days: 14
  # frontend_url: https://mail-admin.example.com
  webauthn:
    enabled: false                   # passkeys (Touch ID, security keys, …)
    # rp_id: app.camelmailer.com
    # rp_origin: https://app.camelmailer.com
app_mail:
  enabled: false                     # send reset/invitation/welcome mail
                                     # through the installation's own pipeline
oidc:
  enabled: false                     # Okta / Entra ID / Google / Keycloak …
saml:
  enabled: false                     # SAML 2.0 SP (signed assertions required)
scim:
  enabled: false                     # SCIM 2.0 provisioning under /scim/v2
web_server:
  cors_origins: []                   # browser origins allowed to call the APIs
```

### `billing:` billing provider (custom builds)

The open-source build carries no payment integration. A billing provider is
plugged in by a separate crate that depends on the `camelmailer` library
crate and builds its own binary (see "Building with a billing provider"
below). Without one, billing stays off whatever this group says: the
billing endpoints report `enabled: false` / answer `403 BillingDisabled`,
and the dashboard's usage page shows usage only.

```yaml
billing:
  enabled: false
  # options:                 # free-form; only the custom provider reads them
  #   api_url: https://billing.example.com
  #   api_key: …             # option values are redacted from debug output
```

`stripe_secret_key` and `portal_return_url` from earlier releases are still
accepted so an existing file keeps loading, and each logs a deprecation
warning at startup. They do nothing.

#### Building with a billing provider

Implement `camelmailer_api::BillingProvider` (customer, checkout, portal,
optional usage view, plans) and start the stock CLI with it:

```rust
fn main() -> std::process::ExitCode {
    camelmailer::main_with(camelmailer::Extensions {
        billing: Some(std::sync::Arc::new(|config: &camelmailer_config::Config| {
            Ok(std::sync::Arc::new(MyProvider::from_options(&config.billing.options)?) as _)
        })),
        ..Default::default()
    })
}
```

The provider is used when `billing.enabled` is true. Checkout, portal and
usage calls return an embed session: a custom element name, the script that
defines it and a short-lived token, which the dashboard renders in place,
plus an optional link fallback. `companions` lists further elements from the
same script that take the same token (an invoice list under a subscription
view, for example). `Extensions::routes` adds HTTP routes to the web server,
and `Extensions::on_start` runs once when the web server starts, inside the
runtime, which is the place to spawn background tasks such as a periodic
sync. `camelmailer::build_state` and `camelmailer_api::build_app` are there
for a binary that wants to assemble the server itself, or for a command of
its own that needs the same storage.

### `retention:` how long data is kept

Days per data class; `0` keeps forever. Every class defaults to `0` except
the API request log (30 days, as before), so an installation that sets
nothing keeps its data.

```yaml
retention:
  message_content_days: 0    # raw MIME, outgoing and inbound
  message_metadata_days: 0   # message rows, deliveries, opens/clicks, tracking tokens
  webhook_log_days: 0
  dmarc_report_days: 0
  auth_event_days: 0
  api_request_days: 30
  batch_size: 1000
```

`camelmailer.message_retention_days` still works as a deprecated alias of
`message_metadata_days` (used only while the new key is `0`). Organizations
can override the first four per class. Expired authentication tokens are
always removed. The full picture, including what each class covers and the
override rules, is in [Data retention](retention.md).

### `worker:` delivery

```yaml
worker:
  threads: 2              # messages one worker process delivers at once
  servers: []             # only these servers (organization/server); empty = all
  exclude_servers: []     # never these servers
  deliver: all            # all | transactional | bulk
```

The queue is fair without any of this. A worker takes turns between the
servers that have mail waiting, so one server's 10,000 queued campaign mails
do not hold back another server's password reset. Within a server,
transactional mail goes first; bulk mail (broadcast streams and campaigns)
follows.

`servers`, `exclude_servers` and `deliver` dedicate a worker to part of the
queue. Each entry is an `organization/server` permalink, and a worker that
names a server that does not exist refuses to start. A setup that keeps a
tenant's newsletters away from everyone's transactional mail:

```yaml
# worker A: everything except the bulk of acme/marketing
worker: { exclude_servers: [acme/marketing] }
# worker B: only acme/marketing
worker: { servers: [acme/marketing], threads: 4 }
```

Or split by kind of mail, for every server: one worker with
`deliver: transactional`, one with `deliver: bulk`.

The environment variables `WORKER_THREADS`, `WORKER_SERVERS`,
`WORKER_EXCLUDE_SERVERS` and `WORKER_DELIVER` override the file (lists are
comma separated), so one config file can start differently scoped worker
containers:

```yaml
services:
  worker-transactional:
    command: worker
    environment: { WORKER_DELIVER: transactional, WORKER_THREADS: "4" }
  worker-bulk:
    command: worker
    environment: { WORKER_DELIVER: bulk }
    deploy: { replicas: 2 }
```

Any number of workers, scoped or not, work on the same queue safely; a
message is handed to exactly one of them.

#### Locks, heartbeats and shutdown

A worker locks the row it works on. While the delivery runs it refreshes
that lock every 30 seconds, so a slow receiver never makes another worker
take the row over. A row whose lock has not been refreshed for the lease is
treated as abandoned by a worker that died, and the next worker takes it.

```yaml
camelmailer:
  queued_message_lock_stale_seconds: 300   # lease; unset = queued_message_lock_stale_days
worker:
  shutdown_grace_seconds: 8                # default; keep it below the stop timeout
```

The lease defaults to `queued_message_lock_stale_days` (one day). Set
`queued_message_lock_stale_seconds` to get mail held by a crashed worker or a
lost host moving again within minutes; values below 90 are raised to 90. The
same lease applies to the webhook queue and the lifecycle outbox.

**Recommended: a lease of about 300 seconds.** The one-day default is kept
only so an upgrade changes nothing. With it, a delivery stopped while talking
to the receiver (a restart, a deploy, a reboot) waits a whole day before it is
tried again. Five minutes is long enough to cover a worker that misses a few
heartbeats under load (they run every 30 seconds) and short enough that such
a delivery moves again soon after the worker comes back. Shorter leases add
no duplicate sends while the worker can reach the database, because the
heartbeat keeps a live worker's rows; they only make a stalled worker lose
its rows sooner.

On SIGTERM or SIGINT the worker stops taking new work and gives the
deliveries in flight `shutdown_grace_seconds` to finish. Whatever is still
running then is stopped:

- A row stopped before it reached a receiver (still loading or signing) goes
  back to the queue without counting an attempt, and the next worker takes it
  straight away.
- A row stopped while talking to the receiver keeps its lock until the lease
  runs out, as after a crash. The receiver may already have accepted it, so it
  is not sent again early. With the default lease of one day that delivery
  waits a day; set `queued_message_lock_stale_seconds` to shorten it.
- A row whose processing failed half way (for example the database was gone
  after the receiver's `250`) also keeps its lock for the lease.

The process manager's stop timeout must stay above the grace period, or the
worker is killed before it has finished. The default of 8 seconds is below
Docker's default stop timeout of 10 seconds; the compose file sets
`stop_grace_period: 30s`, which leaves room to raise the grace period to about
25 seconds for slow receivers.

A panic while processing one row does not stop the worker. The row goes back
to the queue with one attempt counted and waits its backoff, but never less
than the lease, so a message that trips a bug cannot cycle through the
workers.

Delivery stays at-least-once: a worker that dies between the receiver's
`250` and recording it leaves a message that is sent again once its lease
runs out. A short lease makes that no more likely while the worker can reach
the database, because a live worker keeps its locks fresh. If it cannot reach
the database for a whole lease, another worker may take its rows over; the
worker logs a warning naming them when its heartbeat comes back.

### `smtp_client:` outgoing connections

```yaml
smtp_client:
  open_timeout: 30    # seconds for the TCP connection, per remote address
  read_timeout: 300   # seconds for the greeting, a TLS handshake and each reply
  data_timeout: 600   # seconds to send the body and get the reply to its final dot
```

The defaults follow RFC 5321 4.5.3.2. Keep `read_timeout` at minutes, not
seconds: receivers such as Hornetsecurity hold the greeting back on purpose
for a sender they do not know yet, and a worker that gives up after 30
seconds then fails every attempt with `read timeout`.

A server with an IP pool sends from the pool's address, and only to remote
addresses of the same family. A server without one leaves the local address
to the operating system; the worker then tries the remote host's IPv4
addresses first and its IPv6 addresses after, and moves on to the next
address when a connection fails. On a dual-stack host, give the host's IPv6
address a PTR record and an SPF entry before relying on it: Microsoft 365
refuses IPv6 senders without reverse DNS (error S820).

### `lifecycle_webhooks:` installation events

Forwards installation-wide events to outside systems such as a billing or
CRM service. Off while `urls` is empty (the default).

```yaml
lifecycle_webhooks:
  urls: [https://billing.example.com/hooks/camelmailer]
  secret: a-long-random-string       # required when urls is set
```

Events, payload and signature are described in
[Webhooks: installation lifecycle events](webhooks.md#installation-lifecycle-events).

### `captcha:` registration captcha

Protects `POST /api/v2/auth/register`. Off by default.

```yaml
captcha:
  provider: cap                      # none | cap
  endpoint: https://captcha.example.com/<site_key>/
  secret: …                          # never sent to a browser
```

With `cap` the dashboard's sign-up page renders the
[Cap](https://trycap.dev) widget against `endpoint` and sends its token as
`captcha_token`. The server verifies it with
`POST {endpoint}siteverify` and the body `{"secret": …, "response": token}`,
expecting `{"success": true}`. A missing token answers `400
ParameterMissing`, a rejected one `422 CaptchaInvalid`, an unreachable
verifier `503 CaptchaUnavailable`. `GET /api/v2/auth/features` publishes
the provider and the endpoint, never the secret. Any service with the same
siteverify contract works.

## Production checklist

- [ ] `POSTGRES_PASSWORD` strong; database backed up (it holds config *and* mail)
- [ ] `signing.key` generated, mounted, DKIM TXT record published
- [ ] SPF include published for every sending domain
- [ ] `smtp_server.tls_enabled` with a real certificate, and
      `auth_requires_tls` left at its default
- [ ] Reverse proxy (TLS) in front of port 5000; `/health` as the LB probe
- [ ] Port 25 egress open (many clouds block it — or use `smtp_relays`)
- [ ] `RUST_LOG=info`, logs shipped somewhere
- [ ] Admin API keys are database-backed (`make-admin-api-key`), the global
      `admin_api_key` is unset

### Scoped admin API keys

Database-backed admin keys can be **scoped**: created with an
organization (and optionally one server inside it), the key only reaches
that subtree — every other path answers 404. Platform integrations give
each tenant server its own key instead of sharing the installation-wide
one, so a leaked key never crosses tenants:

```bash
camelmailer make-admin-api-key tenant-acme --org acme --server transactional
# or via the API:
curl -s -X POST "$API/api/v2/admin/admin_api_keys" \
  -H "X-Admin-API-Key: $ROOT_KEY" -H "Content-Type: application/json" \
  -d '{"name": "tenant-acme", "organization": "acme", "server": "transactional"}'
```

Scoped keys still count as machine keys inside their subtree (they may
`{"force": true}` domain verification, for example). Global resources —
users, IP pools, key management, `GET /servers/find/{permalink}` — stay
reserved for unscoped keys and instance admins.

## Process model

One binary, four roles — scale each independently:

```
camelmailer web-server    # HTTP APIs; stateless, scale horizontally
camelmailer smtp-server   # SMTP intake; scale behind a TCP LB
camelmailer worker        # delivery; scale by queue depth (SKIP LOCKED —
                          # any number of workers cooperate safely), fair
                          # between servers, scopable per tenant (worker:)
camelmailer initialize    # one-shot migrations (idempotent, run per deploy)
```
