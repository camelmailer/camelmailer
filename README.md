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
| `camelmailer-db` | `lib/postal/message_db/` + the ActiveRecord persistence | ✅ PostgreSQL with row-level security (see below); embedded migrations; implements all storage traits |
| `camelmailer-smtp` | `app/lib/smtp_server/client.rb` + `server.rb`, `script/smtp_server.rb` | ✅ full protocol state machine (see below), tokio TCP server; ⚠️ STARTTLS termination not yet implemented |
| `camelmailer-api` | `app/controllers/admin_api/` | 🚧 conventions complete (auth incl. DB-backed keys, envelope, pagination, errors); organizations + servers resources ported, remaining resources pending |
| `camelmailer` (bin) | `bin/postal` | ✅ CLI dispatcher: `smtp-server`, `web-server`, `initialize` (migrations), `make-admin-api-key`, `version` |

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
- **Honest capability surface.** The Rust SMTP server refuses to start with
  `smtp_server.tls_enabled: true` instead of advertising STARTTLS it cannot
  complete; terminate TLS in front of it until native TLS lands.

## Not yet ported (next phases)

1. **Remaining Admin API resources** — domains, credentials, routes,
   webhooks, suppressions, IP pools, users (the conventions layer they sit on
   is done; storage for domains/routes/credentials already exists in
   `camelmailer-db`).
2. **Worker & delivery pipeline** — queued message dequeuer, SMTP sending,
   bounce handling, webhooks (`app/lib/message_dequeuer`, `app/senders`).
   The cross-tenant queue worker gets a dedicated `BYPASSRLS` role.
3. **Message metadata parity** — the full `messages` schema of
   `lib/postal/message_db/` (spam status, delivery attempts, clicks/opens)
   on the RLS-protected table.
4. **STARTTLS termination** for the SMTP server.
5. **Web UI** — the management interface remains the Rails app for now.

The Ruby application remains fully functional and authoritative while these
phases land; the two run side by side (strangler-fig migration).
