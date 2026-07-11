# CLAUDE.md

Guidance for AI agents working in this repository (CamelMailer).

## What this is

**CamelMailer** — a self-hosted / cloud transactional-email platform:
a Cargo workspace (Rust) for the backend, a Next.js app for web, plus a
transactional template library. Positioning: the self-hosted or EU-cloud
alternative to the big US email providers, focused exclusively on simple
transactional mail. It began as a ground-up Rust rewrite of
[Postal](https://github.com/postalserver/postal) (MIT) and keeps that
attribution, but is an independent project — there is no upstream to track.

## Layout

| Path | What |
|---|---|
| `crates/camelmailer-config` | YAML config + validation (`Config::load_from_env`, `CAMELMAILER_CONFIG_FILE_PATH`, legacy `POSTAL_CONFIG_FILE_PATH` accepted) |
| `crates/camelmailer-core` | Domain model + storage **traits** (`Store` sync/SMTP, `AdminStore`, `ServerStore`, `AuthStore`, `MessageSink`) with in-memory impls for tests; auth primitives (argon2, TOTP, tokens), template renderer (Mustache subset) |
| `crates/camelmailer-db` | PostgreSQL impls of all traits; embedded sqlx migrations (`migrations/*.sql`); tenant isolation via **row-level security** on `messages` |
| `crates/camelmailer-smtp` | SMTP server: pure protocol state machine (no I/O in the session) + tokio listener, STARTTLS via rustls |
| `crates/camelmailer-worker` | Delivery worker: queue dequeuer (SKIP LOCKED), MX/relay SMTP client, DKIM, tracking rewrite, rspamd/ClamAV, webhook queue with RSA signing |
| `crates/camelmailer-api` | axum HTTP API — see surfaces below |
| `crates/camelmailer` | The single binary/CLI: `web-server`, `smtp-server`, `worker`, `initialize`, `make-user`, `make-admin-api-key`, `version` |
| `web/app` | Next.js (App Router) dashboard (`(app)` group, shadcn/ui + TanStack Query); `/` redirects to `/login`; Next proxies `/api` to the backend (`API_PROXY_URL`) |
| `templates/` | 20 ready-to-clone transactional email templates (JSON) + `import.sh` |
| `docs/` | quickstart, configuration, authentication (accounts/RBAC/SSO) |
| `web/app/public/openapi.yaml` | The public OpenAPI 3.0 spec (all 81 endpoints) |

## The three API surfaces

| Surface | Base path | Auth |
|---|---|---|
| Messaging (send, messages, streams, templates, stats) | `/api/v2/server` | `X-Server-API-Key` (an API credential of one mail server) |
| Management (orgs, servers, domains, credentials, …) | `/api/v2/admin` | `X-Admin-API-Key` (machine, full access) **or** `Authorization: Bearer` (user session, RBAC-scoped) |
| Accounts (login, 2FA, invitations, OIDC SSO) | `/api/v2/auth` | none / Bearer |

Conventions (enforced by `crates/camelmailer-api/src/app.rs`): every
response is `{ status, time, data | error }`; error codes are stable
(`InvalidCredentials`, `TOTPRequired`, `AccountLocked`, `Forbidden`, …);
list endpoints paginate `page`/`per_page` (cap 100). RBAC roles:
viewer < member < admin < owner per organization, plus global admins;
non-members get 404 (not 403) for foreign orgs; the last owner is
immovable.

## Storage model

One PostgreSQL database. Config tables are plain; the `messages` table is
protected by **row-level security**: every read/write runs in a
transaction that first sets `set_config('camelmailer.server_id', $1, true)`;
the policy does the filtering — queries carry no `WHERE server_id`.
Never bypass this by adding manual filters or a BYPASSRLS role.

## Development

```bash
docker compose up -d --build     # full stack: db + migrate + web(:5000) + smtp(:25) + worker
cargo test --workspace           # Postgres tests need CAMELMAILER_TEST_DATABASE_URL (role with CREATEDB)
cargo clippy --workspace --all-targets   # CI enforces -D warnings
cargo fmt --all                  # CI enforces --check

cd web/app && pnpm install && pnpm run dev   # Next on :3000, proxies /api to :5000
node e2e/smoke.mjs               # Playwright e2e against the Docker stack (see web/README.md)
```

Bootstrap accounts: `docker compose exec web camelmailer make-user you@x.com First Last --admin`.

## How this codebase was built (and how to keep working on it)

Test-first, throughout. Every behaviour has tests: unit tests in the
crates, trait-level tests against `MemoryStore`, Postgres integration
tests (each test creates a throwaway database and runs migrations),
router tests via `tower::ServiceExt::oneshot`, an OIDC test against a
local mock IdP, and a Playwright e2e for the frontend. Keep that bar:
new behaviour lands with tests; `MemoryStore` and `PgStore` must stay in
behavioural lockstep (same trait tests where practical).

Other conventions:
- New storage needs go on the appropriate **trait** first, then both impls.
- API responses only through the `render_*` helpers; new routes into the
  existing routers with the same envelope.
- Secrets are shown exactly once at creation (keys, invite tokens) and
  stored hashed where applicable (sessions, invitations, resets).
- Frontend pages live in `web/app/src/views/` (client components); route
  files under `src/app/` are thin wrappers. The marketing site lives in a
  separate (private) repository, not here.

## Known deliberate gaps

SAML and SCIM (OIDC is the SSO path), per-domain DKIM keys
(one installation key + selector), billing (planned separately). Legal
pages of the (separately hosted) marketing site are placeholder
templates and marked as such. App-mail delivery of reset/invitation/welcome mail is no longer a
gap: the `app_mail` config group (`enabled`, `server_api_key`,
`from_address`, `from_name`) sends platform mail through the
installation's own pipeline; when disabled, tokens are surfaced to the
operator/frontend as before.
