# CamelMailer

CamelMailer is the incremental Rust rewrite of this Postal fork. It lives as a
Cargo workspace next to the Ruby application and re-implements Postal's
components one by one, test-driven: every ported behaviour is covered by Rust
tests translated from the corresponding RSpec suite before/alongside the
implementation.

## Workspace layout

| Crate | Ports | Status |
|---|---|---|
| `camelmailer-config` | `lib/postal/config_schema.rb`, `lib/postal/config.rb` | ✅ complete (all defaults, YAML loading, `$config-file-root` substitution, legacy `postal:` group alias, `POSTAL_CONFIG_FILE_PATH` fallback) + new `postgres` group |
| `camelmailer-core` | `app/models` (domain model), `app/lib/received_header.rb`, `Postal::Helpers`, token generation | ✅ domain model + storage traits (`Store` for SMTP, async `AdminStore` for the API, `MessageSink`) with in-memory implementations |
| `camelmailer-db` | `lib/postal/message_db/` + the ActiveRecord persistence | ✅ PostgreSQL with row-level security (see below); embedded migrations; message metadata parity (status, spam, deliveries, links/clicks, loads/opens); delivery queue |
| `camelmailer-smtp` | `app/lib/smtp_server/client.rb` + `server.rb`, `script/smtp_server.rb` | ✅ full protocol state machine (see below), tokio TCP server, STARTTLS termination via rustls |
| `camelmailer-worker` | `script/worker.rb`, `app/lib/message_dequeuer`, `app/senders`, `app/lib/postal/message_inspectors`, `dkim_header.rb`, `signer.rb` | ✅ queue dequeuer (SKIP LOCKED), SMTP sending (relays/MX) with opportunistic outbound STARTTLS and IP-pool source addresses, suppression holds, rspamd/ClamAV inspection, DKIM signing, open/click tracking rewrite, HTTP endpoint delivery, webhook queue with retries + RSA signing, delivery recording |
| `camelmailer-api` | `app/controllers/admin_api/` + a native Server API | ✅ **Account API** (`/api/v2/admin`, `X-Admin-API-Key`): auth (incl. DB-backed keys), envelope, pagination, errors; organizations, servers (full config + IP-pool + admin-key management), domains, credentials, routes, webhooks, suppressions, users, IP pools. ✅ **Server API** (`/api/v2/server`, `X-Server-API-Key`): HTTP send, message/delivery/open/click reads, stats + bounces + queue stats, message streams, inbound search/bypass/retry, templates + rendering. ✅ public click/open tracking endpoints |
| `camelmailer` (bin) | `bin/postal` | ✅ CLI dispatcher: `smtp-server`, `web-server`, `worker`, `initialize` (migrations), `make-admin-api-key`, `version` |

## Storage: single PostgreSQL database with row-level security

The Ruby application used MariaDB with a main database plus **one dedicated
database per mail server** for message storage. CamelMailer replaces that
with a **single PostgreSQL database**; tenant (mail-server) isolation on the
shared `messages` table is enforced by the database itself via row-level
security:

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

The SMTP session state machine is a line-for-line port of the Ruby client:
PROXY protocol, HELO/EHLO capability framing, STARTTLS gating of AUTH,
AUTH PLAIN / LOGIN / CRAM-MD5, MAIL FROM (AUTH= stripping), RCPT TO with all
five routing branches (return-path, custom return-path prefix, route domain,
authenticated relay, route match, SMTP-IP fallback with longest-prefix
matching), DATA with dot-unstuffing, header capture with continuation lines,
bare-LF `.` rejection, maximum message size, mail-loop detection, and
From/Sender domain authentication. The Ruby specs in
`spec/lib/smtp_server/client/` are ported to
`crates/camelmailer-smtp/tests/session_tests.rs`.

## Build, test, run

```bash
cd camelmailer
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

Configuration is YAML with the same schema and defaults as the Ruby
`Postal::Config`; an existing `postal.yml` loads unchanged (the `postal:`
group is accepted as an alias for `camelmailer:`, and
`POSTAL_CONFIG_FILE_PATH` still works).

## Architecture notes

- **Pure state machine + trait-backed storage.** The SMTP session performs no
  I/O and no database access; lookups go through the `Store` trait and
  accepted messages through the `MessageSink` trait (`camelmailer-core`).
  `MemoryStore`/`MemorySink` back the tests; the MariaDB implementations are
  the next phase and slot in without touching protocol code.
- **Rebranding.** The SMTP banner reads `ESMTP CamelMailer/<trace-id>`.
  Response texts and status codes are otherwise kept byte-identical to the
  Ruby implementation so existing clients and tests keep matching.
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
  route mail is POSTed to the route's HTTP endpoint (`routes.endpoint_url`,
  a deliberate simplification of Postal's polymorphic endpoints).

## Delivery, inspection and signing

- **Outbound STARTTLS.** The SMTP client upgrades opportunistically when the
  remote MX advertises STARTTLS, honoring `smtp.openssl_verify_mode`
  (`none` accepts any certificate; otherwise the cert is verified against
  the webpki roots). Deliveries over TLS are recorded with `sent_with_ssl`.
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

## Not yet ported

**Web UI** — the management interface remains the Rails app, explicitly out
of scope for the Rust port. Everything else Postal does at the MTA and API
layer is covered here.

**Deferred API capabilities** (documented so a frontend knows they are not
yet available): per-domain DKIM key generation + DNS records and Return-Path
domains (DKIM currently uses one installation key + `dns.dkim_identifier`),
per-address sender signatures, real DNS-based domain verification, webhook
trigger granularity / HTTP auth headers, and template *push* between servers.

The Ruby application remains fully functional and authoritative while these
phases land; the two run side by side (strangler-fig migration).
