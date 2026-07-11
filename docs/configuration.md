# Configuration

CamelMailer reads a single YAML file. Everything has a sensible default —
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

CamelMailer uses one PostgreSQL database for everything; tenant isolation
on message data is enforced *by the database* via row-level security, not
by application code. No per-tenant databases to manage.

### `smtp_server:` — intake

```yaml
smtp_server:
  default_port: 25
  max_message_size: 14        # MB
  tls_enabled: true           # STARTTLS termination
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

### `dns:` — the records you publish

For deliverability you publish, per installation:

| Record | Config key | Example |
|---|---|---|
| MX for inbound | `dns.mx_records` | `mx.example.com` |
| SPF include | `dns.spf_include` | `v=spf1 include:spf.example.com ~all` on sender domains |
| DKIM selector | `dns.dkim_identifier` | `camelmailer._domainkey.<domain>` TXT with the domain key's public part (installation key for pre-existing domains) |
| Return-path | `dns.return_path_domain` | `rp.example.com` |
| Click/open tracking | `dns.track_domain` | CNAME → the web server |

You don't have to assemble these by hand for sending domains:
`GET /api/v2/admin/…/domains/{name}` (and the dashboard's *DNS records*
dialog) returns the exact `verification_record`, `spf_record` and
`dkim_record` to publish. Domain ownership is proven via DNS — publish
the TXT record `_camelmailer-challenge.<domain>` with the value
`camelmailer-verification=<token>` and call
`POST …/domains/{name}/verify`. Operators can skip the check with
`{"force": true}` using the `X-Admin-API-Key` machine key.

### `rspamd:` / `clamav:` — inbound inspection (optional)

Both disabled by default; point them at running rspamd/ClamAV instances to
spam-score and virus-scan inbound mail. Failing messages are held.

### `auth:` / `app_mail:` / `oidc:` / `web_server.cors_origins` — accounts, SSO, browsers

User accounts (sessions, 2FA, lockout), passkeys (WebAuthn), organization
RBAC, invitations, platform email delivery, OIDC single sign-on and CORS
are documented in **[authentication.md](authentication.md)**. Quick anchors:

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
web_server:
  cors_origins: []                   # browser origins allowed to call the APIs
```

### `billing:` — Stripe billing (hosted cloud)

For the hosted cloud offering only. **Self-hosted installations stay
completely billing-free**: the group defaults to `enabled: false`, the
billing endpoints report `enabled: false` / answer `403 BillingDisabled`,
and the dashboard shows no billing UI at all.

```yaml
billing:
  enabled: false                     # cloud only; requires stripe_secret_key
  # stripe_secret_key: sk_live_…     # never logged
  # portal_return_url: https://mail-admin.example.com   # default: auth.frontend_url
```

When enabled, organization admins/owners get a **Billing Portal** entry in
the organization settings. The backend creates the Stripe customer lazily
on first use (`POST /api/v2/admin/organizations/{org}/billing/portal`,
idempotent — an existing customer is reused) and redirects into Stripe's
billing portal. Stripe outages surface as the stable error code
`BillingUnavailable`; Stripe error details are only ever logged.

## Production checklist

- [ ] `POSTGRES_PASSWORD` strong; database backed up (it holds config *and* mail)
- [ ] `signing.key` generated, mounted, DKIM TXT record published
- [ ] SPF include published for every sending domain
- [ ] `smtp_server.tls_enabled` with a real certificate
- [ ] Reverse proxy (TLS) in front of port 5000; `/health` as the LB probe
- [ ] Port 25 egress open (many clouds block it — or use `smtp_relays`)
- [ ] `RUST_LOG=info`, logs shipped somewhere
- [ ] Admin API keys are database-backed (`make-admin-api-key`), the global
      `admin_api_key` is unset

## Process model

One binary, four roles — scale each independently:

```
camelmailer web-server    # HTTP APIs; stateless, scale horizontally
camelmailer smtp-server   # SMTP intake; scale behind a TCP LB
camelmailer worker        # delivery; scale by queue depth (SKIP LOCKED —
                          # any number of workers cooperate safely)
camelmailer initialize    # one-shot migrations (idempotent, run per deploy)
```
