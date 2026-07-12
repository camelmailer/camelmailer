<p align="center">
  <img src="web/app/public/camelmailer-logo.png" alt="CamelMailer" width="420">
</p>

# CamelMailer

**A headless, API-first mail delivery platform in Rust.** SMTP in and out,
HTTP APIs for everything — sending, templates, message streams, statistics,
bounces, inbound routing, webhooks, tracking — backed by a single PostgreSQL
database with row-level-security tenant isolation. User accounts with
2FA, organization-level RBAC, invitations, OIDC single sign-on, an auth
audit log and CORS make it frontend- and enterprise-ready out of the box.
One small binary runs every process role. MIT licensed; born as a full
Rust rewrite of [Postal](https://github.com/postalserver/postal).

**Web dashboard included:** `web/app` is a Next.js (shadcn/ui) admin app
covering the entire API — registration and login with 2FA/SSO,
organizations and roles, servers with all resources, sending and message
browsing. The public [OpenAPI spec](web/app/public/openapi.yaml) documents
all 74 endpoints. A library of 20 ready-to-clone transactional email
templates lives in [templates/](templates/README.md). See
[web/README.md](web/README.md).

## Install

Three equivalent ways to run the full stack — pick one:

| | Best for | You need |
|---|---|---|
| **[Prebuilt Docker](#prebuilt-docker-image-ghcr)** | fastest start, no toolchain | Docker only |
| **[Debian / Ubuntu](#debian--ubuntu-deb)** | bare-metal / VM, systemd | a PostgreSQL |
| **[From source](#from-source)** | hacking on CamelMailer | Docker (or Rust) |

### Prebuilt Docker image (GHCR)

One file, no repo clone — pulls `ghcr.io/camelmailer/camelmailer`:

```bash
mkdir camelmailer && cd camelmailer
curl -fsSLO https://raw.githubusercontent.com/camelmailer/camelmailer/main/install/docker-compose.yml
echo "POSTGRES_PASSWORD=$(openssl rand -hex 16)" > .env
docker compose up -d
docker compose exec web camelmailer make-user you@example.com Ada Ops --admin
```

Multi-arch (amd64 + arm64); pin a release with `CAMELMAILER_VERSION=vX.Y.Z`
in `.env`.
`latest` is the latest tagged release; `edge` follows `main`.

### Debian / Ubuntu (.deb)

Native amd64/arm64 packages with systemd units, from the
[releases page](https://github.com/camelmailer/camelmailer/releases):

```bash
sudo dpkg -i camelmailer_*.deb        # binary, units, /etc/camelmailer/
sudo editor /etc/camelmailer/camelmailer.yml   # point it at your PostgreSQL
sudo systemctl enable --now camelmailer-web camelmailer-smtp camelmailer-worker
```

Step-by-step (incl. PostgreSQL setup and bootstrap):
**[docs/install-deb.md](docs/install-deb.md)**.

### From source

```bash
git clone https://github.com/camelmailer/camelmailer && cd camelmailer
cp .env.example .env          # set POSTGRES_PASSWORD
docker compose up -d --build
curl http://localhost:5000/health

docker compose exec web camelmailer make-admin-api-key ops
```

Each path gives you PostgreSQL + migrations + the HTTP API (`:5000`) + SMTP
(`:25`) + the delivery worker. Follow **[docs/quickstart.md](docs/quickstart.md)**
for the five-minute zero-to-first-mail walkthrough, and
**[docs/configuration.md](docs/configuration.md)** for DKIM, DNS records,
TLS, and the production checklist. Accounts, roles and SSO are covered in
**[docs/authentication.md](docs/authentication.md)**.

## How it's built

Test-first, throughout: protocol state machines, storage traits with
in-memory and PostgreSQL implementations kept in behavioural lockstep,
router tests for every endpoint, and PostgreSQL integration tests for the
row-level-security tenant isolation. CI enforces `cargo fmt`,
`clippy -D warnings` and the full suite; releases are cut from tags and
refuse to publish unless everything is green (see
[CHANGELOG.md](CHANGELOG.md)).

## Workspace layout

| Crate | What |
|---|---|
| `camelmailer-config` | YAML configuration with validated schema and full defaults; `$config-file-root` substitution; Postal-compatible config files load unchanged |
| `camelmailer-core` | Domain model + storage traits (`Store` for SMTP, async `AdminStore`, `ServerStore`, `AuthStore`, `MessageSink`) with in-memory implementations for tests; auth primitives (argon2, TOTP, tokens); safe Mustache-subset template renderer |
| `camelmailer-db` | PostgreSQL implementations of all traits; embedded sqlx migrations; row-level-security tenant isolation (see below); delivery queue |
| `camelmailer-smtp` | SMTP server: pure protocol state machine (no I/O in the session), tokio TCP listener, STARTTLS termination via rustls |
| `camelmailer-worker` | Delivery worker: `SKIP LOCKED` dequeuer, SMTP sending (relays/MX) with opportunistic outbound STARTTLS and IP-pool source addresses, suppression holds, rspamd/ClamAV inspection, DKIM signing, open/click tracking rewrite, HTTP endpoint delivery, webhook queue with retries + RSA signing |
| `camelmailer-api` | axum HTTP API: **Server API** (`/api/v2/server`, sending + message data), **Management API** (`/api/v2/admin`, all resources, RBAC-scoped for user sessions), **Accounts API** (`/api/v2/auth`, login/2FA/registration/resets/invitations/OIDC), public tracking endpoints, platform app-mail |
| `camelmailer` (bin) | The single binary/CLI: `web-server`, `smtp-server`, `worker`, `initialize`, `make-user`, `make-admin-api-key`, `version` |

## Storage: single PostgreSQL database with row-level security

CamelMailer uses a **single PostgreSQL database**; tenant (mail-server)
isolation on the shared `messages` table is enforced by the database
itself via row-level security:

- Every message read/write runs inside a transaction that establishes the
  tenant context first: `set_config('camelmailer.server_id', $1, true)`.
- The policy (`migrations/0002_rls.sql`) filters reads (`USING`) and rejects
  writes (`WITH CHECK`) outside that context. Queries carry **no**
  `WHERE server_id` filters — isolation does not depend on application code
  remembering to scope.
- `FORCE ROW LEVEL SECURITY` keeps even the table owner subject to the
  policy; only a superuser or an explicit `BYPASSRLS` role (reserved for a
  future cross-tenant queue worker) can cross tenants.

The RLS behaviour is covered by integration tests against a real PostgreSQL
(`crates/camelmailer-db/tests/pg_tests.rs`): tenant-scoped reads, hidden
rows without context, rejected cross-tenant writes, and unfiltered
`UPDATE`/`DELETE` confined to the tenant — plus an end-to-end test driving a
full SMTP session into Postgres. Set `CAMELMAILER_TEST_DATABASE_URL` (a role
with CREATEDB) to run them; they skip otherwise. Each test creates its own
throwaway database and runs the embedded migrations.

The SMTP session is a pure protocol state machine covering PROXY protocol,
HELO/EHLO capability framing, STARTTLS gating of AUTH,
AUTH PLAIN / LOGIN / CRAM-MD5, MAIL FROM (AUTH= stripping), RCPT TO with all
five routing branches (return-path, custom return-path prefix, route domain,
authenticated relay, route match, SMTP-IP fallback with longest-prefix
matching), DATA with dot-unstuffing, header capture with continuation lines,
bare-LF `.` rejection, maximum message size, mail-loop detection, and
From/Sender domain authentication — backed by an extensive session test
suite (`crates/camelmailer-smtp/tests/session_tests.rs`).

## Build, test, run

```bash
cargo test              # all crates (Postgres tests skip without CAMELMAILER_TEST_DATABASE_URL)
CAMELMAILER_TEST_DATABASE_URL=postgres://user:pass@localhost:5432/cm_test cargo test
cargo clippy --workspace --all-targets

cargo run -p camelmailer -- version
CAMELMAILER_CONFIG_FILE_PATH=config/camelmailer.yml cargo run -p camelmailer -- initialize
CAMELMAILER_CONFIG_FILE_PATH=config/camelmailer.yml cargo run -p camelmailer -- make-admin-api-key ops
CAMELMAILER_CONFIG_FILE_PATH=config/camelmailer.yml cargo run -p camelmailer -- smtp-server
CAMELMAILER_CONFIG_FILE_PATH=config/camelmailer.yml cargo run -p camelmailer -- web-server
```

With `postgres.enabled: true` (or `DATABASE_URL` set) both servers run
against PostgreSQL; otherwise they fall back to non-persistent in-memory
storage and log a warning.

Configuration is one YAML file — `config/camelmailer.example.yml`
documents every group. For migrations, Postal-compatible config files load
unchanged (`postal:` group alias, `POSTAL_CONFIG_FILE_PATH`).

## Architecture notes

- **Pure state machine + trait-backed storage.** The SMTP session performs no
  I/O and no database access; lookups go through the `Store` trait and
  accepted messages through the `MessageSink` trait (`camelmailer-core`).
  `MemoryStore`/`MemorySink` back the tests; the PostgreSQL implementations
  in `camelmailer-db` implement the same traits, so protocol code never
  touches persistence details.
- **STARTTLS termination.** With `smtp_server.tls_enabled: true` the server
  loads the configured certificate/key, advertises STARTTLS (and withholds
  AUTH) on plaintext sessions, upgrades the socket via rustls on request and
  continues the same session encrypted. Messages received after the upgrade
  are marked `received_with_ssl`.
- **Delivery pipeline.** Accepted messages are enqueued in the same
  transaction that stores them. The worker dequeues with
  `FOR UPDATE SKIP LOCKED`, enters the owning tenant's RLS context per
  message (no BYPASSRLS role needed), checks the suppression list, sends via
  configured relays or MX lookup, retries soft failures with exponential
  backoff, records every attempt in `deliveries`, and fires webhooks
  (MessageSent/MessageDelayed/MessageDeliveryFailed/MessageHeld). Incoming
  route mail is POSTed to the route's HTTP endpoint (`routes.endpoint_url`).

## Delivery, inspection and signing

- **Outbound STARTTLS.** For **direct-to-MX** delivery the SMTP client
  upgrades opportunistically when the remote MX advertises STARTTLS but does
  **not** verify the certificate (like `smtp_tls_security_level = may`): a
  foreign MX's certificate is not issued for our benefit, so requiring a
  webpki trust chain would fail against practically every real MX. If the
  handshake fails, delivery falls back to a fresh plaintext connection rather
  than stalling. `smtp.openssl_verify_mode` governs certificate verification
  for **configured relays only** (a smarthost with a known identity): `peer`
  verifies against the webpki roots, `none` accepts any certificate; `smtps://`
  relays use implicit TLS with verification by default. Deliveries over TLS
  are recorded with `sent_with_ssl`.
- **IP-pool source addresses.** A server can be assigned an IP pool; the
  worker binds the highest-priority IPv4 of that pool as the local source
  address for outbound connections.
- **DKIM.** Outgoing mail with an authenticated domain is signed at delivery
  time (RFC 6376, rsa-sha256, relaxed/relaxed) with the installation signing
  key and the `dns.dkim_identifier` selector; the stored copy stays unsigned.
- **Inspection.** Incoming mail is scored by rspamd and scanned by ClamAV
  (both opt-in); a virus hit or a spam-failure score holds the message.
- **Tracking.** HTML links in outgoing mail are rewritten to
  `/track/c/<token>` redirects and an open pixel is injected; the public,
  unauthenticated tracking endpoints resolve tokens and record clicks/opens
  into the RLS-protected tables.
- **Webhooks.** Events are queued, delivered with retries and exponential
  backoff, optionally RSA-signed (`X-CamelMailer-Signature`), and every
  attempt is written to a tenant-scoped audit log.

## Headless API: Account + Server scopes

CamelMailer is API-first — the whole platform is drivable over HTTP so a
frontend (e.g. React) can be built on top. There are two token scopes, both
returning the native `{ status, time, data | error }` envelope with
snake_case fields and `{ page, per_page, total, total_pages }` pagination:

- **Account API** — `X-Admin-API-Key` → `/api/v2/admin/...`. Org/server
  management: organizations, servers (create/update/suspend, full config —
  open/click tracking, spam thresholds, hook URLs, inbound domain, colour,
  IP pool, default stream), domains, credentials, routes, webhooks,
  suppressions, users, IP pools, and admin-API-key management.
- **Server API** — `X-Server-API-Key` → `/api/v2/server/...`. A per-server
  token (a `credentials` record of type `API`) implies exactly one server;
  every request is scoped to it and message-data queries enter that server's
  RLS tenant context. It is a sibling router — **not** under admin auth.

Server API surface:

| Area | Endpoints |
|---|---|
| Self / probe | `GET /`, `GET /ping` |
| Send | `POST /messages`, `POST /messages/batch` — builds MIME, authorises the From-domain, fans out one stored message per recipient; DKIM + tracking are applied by the worker at delivery time |
| Send with template | `POST /messages/with_template`, `POST /messages/with_template/batch` |
| Messages | `GET /messages` (filter: `scope/status/tag/stream/query`, paged), `GET /messages/{id}` (+ deliveries), `/deliveries`, `/opens`, `/clicks`, `/raw` (base64; 404 in privacy mode) |
| Statistics | `GET /stats` (status + open/click counters, `?from/&to`), `GET /stats/deliveries` (outbound queue depth) |
| Bounces | `GET /bounces`, `GET /bounces/{id}` |
| Message streams | `GET/POST /streams`, `GET/PATCH /streams/{permalink}`, `POST /streams/{permalink}/archive` |
| Inbound | `GET /inbound`, `GET /inbound/{id}`, `POST /inbound/{id}/bypass`, `POST /inbound/{id}/retry` |
| Templates | `GET/POST /templates`, `GET/PATCH /templates/{permalink}`, `POST /templates/{permalink}/archive`, `POST /templates/{permalink}/render` (dry-run) |

Templates render with a small **Mustache subset** (`camelmailer-core::template`):
`{{ var }}` (HTML-escaped), `{{{ var }}}`/`{{& var }}` (raw), `{{# s }}`/`{{^ s }}`
sections over arrays/objects, dotted paths and `.`. No partials, lambdas, or
IO; output size and section-nesting depth are capped because the model is
untrusted end-user data. Tenant isolation is enforced everywhere — a token
for server A can never read or mutate server B's data — and is covered by
both router tests and Postgres RLS tests.

## Roadmap / deliberate gaps

Everything above ships today, including the web dashboard. Deliberately
deferred (documented so integrators know what not to expect yet):

- **Per-domain DKIM keys** — signing currently uses one installation key
  with the `dns.dkim_identifier` selector.
- **DNS-based domain verification** — verifying a sending domain is an
  explicit admin action today; automated record probing is planned.
- **Webhook trigger granularity and custom HTTP auth headers.**
- **Per-address sender signatures** and **template push between servers**.
- **SAML / SCIM** (OIDC is the SSO path) and **WebAuthn**.
- **Billing** — planned separately for the hosted cloud.

## License

MIT — see [LICENSE](LICENSE). CamelMailer began as a Rust rewrite of
[Postal](https://github.com/postalserver/postal) (also MIT); portions of the
design and behaviour derive from it.
