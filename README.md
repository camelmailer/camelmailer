# CamelMailer

CamelMailer is the incremental Rust rewrite of this Postal fork. It lives as a
Cargo workspace next to the Ruby application and re-implements Postal's
components one by one, test-driven: every ported behaviour is covered by Rust
tests translated from the corresponding RSpec suite before/alongside the
implementation.

## Workspace layout

| Crate | Ports | Status |
|---|---|---|
| `camelmailer-config` | `lib/postal/config_schema.rb`, `lib/postal/config.rb` | ✅ complete (all defaults, YAML loading, `$config-file-root` substitution, legacy `postal:` group alias, `POSTAL_CONFIG_FILE_PATH` fallback) |
| `camelmailer-core` | `app/models` (domain model), `app/lib/received_header.rb`, `Postal::Helpers`, token generation | ✅ domain model + storage traits with in-memory implementation |
| `camelmailer-smtp` | `app/lib/smtp_server/client.rb` + `server.rb`, `script/smtp_server.rb` | ✅ full protocol state machine (see below), tokio TCP server; ⚠️ STARTTLS termination not yet implemented |
| `camelmailer-api` | `app/controllers/admin_api/` | 🚧 conventions complete (auth, envelope, pagination, errors); organizations + servers resources ported, remaining resources pending |
| `camelmailer` (bin) | `bin/postal` | ✅ CLI dispatcher: `smtp-server`, `web-server`, `version` |

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
cargo test              # all crates, 95+ tests
cargo clippy --workspace --all-targets

cargo run -p camelmailer -- version
CAMELMAILER_CONFIG_FILE_PATH=config/camelmailer.yml cargo run -p camelmailer -- smtp-server
CAMELMAILER_CONFIG_FILE_PATH=config/camelmailer.yml cargo run -p camelmailer -- web-server
```

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

1. **MariaDB persistence** — `Store`/`MessageSink` implementations against the
   main DB and the per-server message databases (`lib/postal/message_db/`),
   including schema migrations.
2. **Remaining Admin API resources** — domains, credentials, routes,
   webhooks, suppressions, IP pools, users (the conventions layer they sit on
   is done).
3. **Worker & delivery pipeline** — queued message dequeuer, SMTP sending,
   bounce handling, webhooks (`app/lib/message_dequeuer`, `app/senders`).
4. **STARTTLS termination** for the SMTP server.
5. **Web UI** — the management interface remains the Rails app for now.

The Ruby application remains fully functional and authoritative while these
phases land; the two run side by side (strangler-fig migration).
