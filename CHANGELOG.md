# Changelog

All notable changes to CamelMailer are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/):
until 1.0.0, minor versions may contain breaking changes (called out
explicitly below).

Releases are cut from tags: `git tag vX.Y.Z && git push origin vX.Y.Z`.
The release workflow refuses to publish unless the tag, the workspace
version in `Cargo.toml` and a matching section in this file agree, and
unless the full test suite (including the PostgreSQL row-level-security
integration tests) is green.

## [Unreleased]

## [0.4.0] - 2026-07-12

### Added

- **Redesigned dashboard** on the shadcn dashboard-01 layout: collapsible
  icon sidebar with an organization switcher and per-server navigation, a
  ⌘K command palette, breadcrumb header, dark mode, and empty states with
  one-click actions across every resource list. New accounts land in a
  ready-made workspace and are guided by an onboarding checklist.
- **Activity, messages & analytics**: a lifecycle event stream with
  omni-search and time/status/tag/stream filters; a message detail with a
  Sent→Delivered→Opened→Clicked timeline and Preview / Plain text / HTML /
  Raw / Insights tabs (deep-linkable); dashboard KPI cards and a stacked
  delivery chart with bounce/complaint risk lines; an API request log view.
- **Deliverability & trust**: a domain detail with grouped DNS records
  (verification / SPF / DKIM), per-record status and a health check; a
  DMARC compliance view; a recipient detail with a per-address event
  timeline and SMTP delivery proof; suppression reactivation and CSV export.
- **Tools**: a template gallery with a split code/live-preview editor and a
  one-click starter library, a webhook editor with event selection, custom
  headers, a live example payload and test-send, an API-keys view with
  "last used" plus a copy-first SMTP settings panel, and a per-language
  Setup tab with runnable snippets.
- **Polish**: a deliverability insights coach on each message, a route-aware
  `</>` API code panel (curl + SDK snippets), a statistics view with
  sentence KPIs and a clickable bounce breakdown, and a usage & billing
  page (Stripe portal on the cloud).
- **Bootstrap workspace** (`auth.bootstrap_workspace`, default off; meant
  for the cloud): brand-new accounts — self-registration and the very
  first OIDC/SAML/social-SSO auto-provisioning — automatically get an
  organization "\<FirstName>'s Team" (user as owner; permalink slugged,
  collisions suffixed `-2`, `-3`, …), a server `production` and, for
  registration only, an API credential `default`. `POST
  /api/v2/auth/register` then returns `workspace: { organization, server,
  api_key }` — the key appears exactly once, there; SSO provisioning
  creates no credential (no response channel to show a key once).
  Bootstrap failures are logged and never fail the account creation.
- **Org-wide 2FA enforcement** (Postmark pattern):
  `organizations.require_two_factor` (migration 0024), readable via `GET`
  and settable via the new `PATCH /api/v2/admin/organizations/{org}`
  (owner-only). While on, session users without an active second factor
  (activated TOTP or ≥ 1 passkey) get a stable `403 TwoFactorEnforced` on
  every resource of the organization — enforced centrally in the RBAC
  layer, global admins included (no backdoor); admin API keys are
  unaffected and non-members keep their indistinguishable 404. New
  `AuthStore::user_has_two_factor` on both stores backs the check. The
  web app gains an owner-only "Require two-factor authentication" toggle
  in Organization → Settings and a full-page "Enable 2FA to access this
  organization" card (linking to Account → Security) whenever the API
  answers `TwoFactorEnforced`.
- **Public share links for message details** —
  `POST /api/v2/server/messages/{id}/share` (`expires_in_hours`, default
  48, max 168) returns a one-time URL `<frontend_url>/share/m/<token>`;
  the token is random and stored **only as a SHA-256 hash** (table
  `message_shares`, a cross-tenant lookup like tracking tokens). The
  unauthenticated `GET /api/v2/share/messages/{token}` serves the full
  support context — message, deliveries, opens, clicks and the decoded
  HTML/text bodies — while the link is valid; unknown tokens answer
  `404 NotFound`, expired ones the stable `404 ShareLinkExpired`. The
  dashboard grows a "Share" action in the message detail (expiry picker,
  generated URL with copy) and a public read-only page at
  `/share/m/<token>` (meta, delivery timeline, HTML/text tabs).

- **Deliverability insights per message** —
  `GET /api/v2/server/messages/{id}/insights` evaluates a rule catalog
  from stored data plus one live DNS lookup: plain-text part present,
  subject present and ≤ 78 characters, no no-reply From, links and
  images on the From domain (URL shorteners called out), body under
  100 KB, From domain verified, DMARC record published (a DNS failure
  skips the check instead of failing the request), and
  DKIM active (domain or installation key). The dashboard shows an
  "Insights" tab in the message detail with DOING GREAT / IMPROVE
  sections, a warning-count badge and the generation timestamp.

- **Webhook test sends** —
  `POST /api/v2/admin/organizations/{org}/servers/{server}/webhooks/{id}/test`
  with `{ "event": "MessageSent" }` synchronously delivers a realistic
  sample payload (marked `"test": true`) to the webhook URL, including
  the custom headers and the RSA signature exactly as the worker sends
  them (10 s timeout), and reports
  `{ delivered, status_code, duration_ms, error? }`. The dashboard adds
  a "Send test" action per webhook with event picker, result pill and
  the sample payload (JSON, copyable).
- **Observability for the messaging API** — four pieces:
  **Credential usage**: credentials now report `last_used_at` in the
  management API (`GET …/credentials`), stamped on every successful
  API-key authentication and SMTP AUTH; null until first use.
  **Tags as a first-class dimension**: `GET /api/v2/server/tags` lists
  the tags used in the last 30 days with message counts (most used
  first, tenant-scoped), and `GET /api/v2/server/stats?tag=` scopes
  every counter (including opens/clicks) to one tag
  (`GET /api/v2/server/messages?tag=` already filtered).
  **Bounce classification**: terminally failed deliveries and processed
  inbound bounces (DSNs) are classified `hard` (5xx / enhanced status
  `5.x.x` — permanent), `soft` (4xx / `4.x.x` — retries exhausted) or
  `undetermined` (connection errors, unparsable output; DSNs are read
  from their `Status:`/`Diagnostic-Code:` fields only). The category is
  persisted on the message (`bounce_category`, migration 0027), exposed
  on messages/bounces and broken down in stats as
  `bounces: { hard, soft, undetermined }` — unclassified bounces count
  as undetermined.
  **API request log**: every authenticated request to
  `/api/v2/server/*` is logged asynchronously (fire-and-forget — it can
  neither slow down nor fail the request) with method, path (no query
  string), status code, duration and a truncated user agent — never
  bodies or API keys (new `api_requests` table, migration 0028).
  `GET /api/v2/server/logs` lists the entries newest first, paginated,
  filterable by status class (`2xx`…`5xx`), method and time window;
  worker housekeeping deletes entries older than 30 days (hourly).

## [0.3.0] - 2026-07-11

### Added

- **DMARC monitoring** (see `docs/dmarc.md`) — three pieces:
  a live **domain health check**
  (`GET /api/v2/admin/organizations/{org}/servers/{server}/domains/{name}/health`)
  that resolves SPF, DKIM (`<selector>._domainkey.<domain>`) and DMARC
  (`_dmarc.<domain>`) via DNS, grades each check ok/warning/missing with
  the found records, the expected value and concrete problems, and
  recommends the next policy step (no DMARC → `p=none` with `rua=`;
  `p=none` with high compliance on recent reports → `p=quarantine`;
  `p=quarantine` with high compliance → `p=reject`).
  **Aggregate-report ingestion (RUA)**: inbound routes may target
  `internal://dmarc-reports` (the only accepted non-HTTP endpoint); the
  worker then parses arriving mail as RFC 7489 aggregate reports
  (`.xml`, `.xml.gz`, `.zip` attachments or XML directly in the body)
  into the new RLS-protected `dmarc_reports` / `dmarc_report_records`
  tables (tenant = server, FORCE row-level security like `messages`);
  unparseable reports are held like any undeliverable inbound message —
  they never crash the worker.
  **Compliance API + dashboard**: `GET /api/v2/server/dmarc/summary`
  (pass rate, top-20 sources with alignment percentages, dispositions),
  `GET /api/v2/server/dmarc/reports` (paginated) and
  `GET /api/v2/server/dmarc/reports/{id}` (rows included), plus a
  "DMARC" tab per server in the dashboard with the health traffic
  lights, the compliance summary, the sources table and the RUA setup
  hint. Ingested report messages get the new `Processed` status.

- **Stripe billing for the hosted cloud** — a new `billing` config group
  (`enabled`, `stripe_secret_key`, `portal_return_url`; the secret is
  never logged). When enabled, organization admins/owners get
  `GET /api/v2/admin/organizations/{org}/billing` (billing status) and
  `POST /api/v2/admin/organizations/{org}/billing/portal`, which lazily
  creates the Stripe customer (idempotent — an existing customer is
  reused; the id is stored in `organizations.billing_customer_id` only
  after Stripe reported success) and returns a billing-portal URL the
  dashboard redirects to ("Billing Portal" in the organization
  settings). Stripe failures surface as the stable `BillingUnavailable`
  error code; disabled billing answers `403 BillingDisabled`.
  **Self-hosted installations are unaffected**: billing defaults to
  disabled, the status endpoint reports `enabled: false` and the
  dashboard shows no billing UI at all.
- **Webhook trigger granularity + custom headers** — webhooks carry an
  `events` list (any of `MessageSent`, `MessageDelayed`,
  `MessageDeliveryFailed`, `MessageHeld`; empty = all events, so existing
  webhooks keep firing unchanged) and a `headers` map of extra HTTP
  headers (e.g. `Authorization`) set on every delivery request alongside
  the signature headers. The worker filters events at enqueue time;
  create/update via the management API validates event names (unknown
  name → `ValidationError` listing the valid ones) and header syntax
  (values are never logged or echoed). New `PATCH …/webhooks/{id}`;
  the dashboard's webhook form gained event checkboxes and a header
  key/value list.
- **Per-address sender signatures** — `sender_addresses`: a server may
  send From an exact address once its owner confirms it, without a
  verified sending domain. `POST/GET/DELETE
  /api/v2/admin/organizations/{org}/servers/{server}/sender_addresses`
  creates the address with a hashed verification token and (with
  `app_mail` enabled) emails a confirmation link
  (`{frontend_url}/sender-addresses/confirm?token=…`) to exactly that
  address — otherwise the token is returned to the operator once.
  Public `POST /api/v2/auth/sender-addresses/confirm` redeems the
  single-use token. From-authorization on both the HTTP and SMTP send
  paths now accepts a verified sending domain OR a confirmed sender
  address (exact, case-insensitive match). Dashboard: a "Senders" tab
  per server plus a public confirm page.
- **Template push between servers** — `POST
  /api/v2/admin/organizations/{org}/servers/{server}/templates/{permalink}/copy_to`
  with `{ "target_server": "<permalink>" }` copies a template to another
  server of the same organization (member role or above; a target
  outside the organization is an indistinguishable 404). An existing
  permalink on the target is a 422 `ValidationError` unless
  `{ "overwrite": true }` is passed. Dashboard: a "Copy to server…"
  action in the Templates view.
- **Per-domain DKIM keys** — every domain created through the API gets
  its own RSA-2048 signing key; the worker signs with the domain key
  when present and falls back to the installation key
  (`camelmailer.signing_key_path`) otherwise — the fallback stays valid
  forever, so existing domains keep working. The domain endpoints return
  a ready-to-publish `dkim_record`
  (`<dns.dkim_identifier>._domainkey.<domain>`); the private key is
  never exposed.
- **DNS-based domain verification** — domains carry a stable
  `verification_token`; `GET …/domains/{name}` returns
  `verification_record` (`_camelmailer-challenge.<domain>` TXT with
  `camelmailer-verification=<token>`) and an `spf_record`
  recommendation. `POST …/domains/{name}/verify` now checks the TXT
  record live (hickory-resolver) and answers 422 `ValidationError`
  naming the missing record on failure; operators with the
  `X-Admin-API-Key` machine key may skip the check with
  `{"force": true}` (user sessions get 403). The dashboard shows the
  three records with copy buttons and surfaces the API's error message
  on Verify.
- **Social sign-in with multiple providers (Google / Microsoft /
  GitHub)** — a new `auth.sso_providers` list configures any number of
  "Continue with …" providers side by side, each served under
  `GET /api/v2/auth/sso/{id}/start` and `…/{id}/callback` (register the
  redirect URI `https://<host>/api/v2/auth/sso/{id}/callback` with the
  provider). `type: oidc` runs the authorization-code flow with PKCE
  and full ID-token validation like the enterprise `oidc` group (which
  keeps working unchanged alongside); Microsoft's multi-tenant `common`
  issuer is supported by validating the per-tenant `iss` claim against
  the `https://login.microsoftonline.com/<tenant-guid>/v2.0` pattern
  while signature, `aud`, `exp` and `nonce` stay strict. `type: github`
  runs GitHub's OAuth2 flow and requires a verified email address
  (`422 SSOEmailUnavailable` otherwise). Per provider:
  `allowed_email_domains` and `auto_provision` (default true). Accounts
  link per provider (new `sso_identities` table), so one account can
  hold Google, Microsoft and GitHub links at once. The new public
  `GET /api/v2/auth/features` endpoint lists the configured providers
  (`{id, name, type}` — never secrets) and drives the new
  "Continue with {name}" / "Sign up with {name}" buttons on the login
  and registration pages.
- **Passkeys (WebAuthn)** — users can register passkeys (Touch ID,
  Windows Hello, security keys) on Account → Security and sign in with
  them (`/api/v2/auth/webauthn/*`, built on `webauthn-rs`). Opt-in via
  the `auth.webauthn` group (`enabled`, `rp_id`, `rp_origin`,
  `rp_name`); while disabled the endpoints answer `403 WebAuthnDisabled`.
  Login start/finish is enumeration-safe (generic response with
  deterministic fake credential ids for unknown addresses), ceremony
  state is server-side, short-lived and single-use, signature counters
  are verified and persisted (clone detection), and passkey logins,
  registrations and deletions land on the audit log.
- **`GET /api/v2/auth/features`** — public discovery of the optional
  sign-in features (`webauthn`, `registration`, `oidc {enabled, name}`);
  the login page uses it to decide which buttons/links to render.
- **SAML 2.0 single sign-on** — CamelMailer can act as a SAML service
  provider (`saml` config group: `enabled`, `name`, `idp_sso_url`,
  `idp_certificate`, `sp_entity_id`, `auto_provision`,
  `allowed_email_domains`). HTTP-Redirect binding for the AuthnRequest,
  HTTP-POST binding at `/api/v2/auth/saml/acs`, SP metadata at
  `/api/v2/auth/saml/metadata`. Responses are strictly validated:
  rsa-sha256 XML signature against the configured IdP certificate
  (unsigned assertions are rejected, `ds:KeyInfo` is ignored),
  audience, destination, `InResponseTo` against stored single-use
  request state, `NotBefore`/`NotOnOrAfter`, and an assertion-id replay
  cache. Accounts resolve by email with optional auto-provisioning;
  the login page shows a "Sign in with <name>" button when enabled.
- **SCIM 2.0 provisioning** (RFC 7643/7644, Users core) under
  `/scim/v2` (`scim` config group: `enabled`, `bearer_token`).
  Discovery (`ServiceProviderConfig`, `ResourceTypes`, `Schemas`),
  Users CRUD with `startIndex`/`count` pagination and the
  `userName eq "…"` filter, PATCH/PUT of `active`, `userName` and
  `name`; `DELETE` deactivates instead of hard-deleting. Deactivated
  accounts (`active: false`, new `user_auth.disabled` flag) are blocked
  from password, OIDC and SAML login and password resets, and their
  sessions are revoked — login answers the new stable error code
  `AccountDisabled`.

## [0.2.0] - 2026-07-11

### Added

- **Multi-port SMTP intake** — the SMTP server can listen on several
  ports at once via `smtp_server.listeners` (each `{ port, mode }`),
  alongside `default_port`. Mode `smtp` is plaintext + optional STARTTLS
  (587-style submission); mode `smtps` is implicit TLS from the first
  byte (port 465, requires `tls_enabled`). Sessions on `smtps` start in
  the TLS state: AUTH available immediately, messages marked as received
  over TLS. Defaults to no extra listeners (unchanged behaviour).
- **Relay URLs with port, TLS mode and credentials** —
  `camelmailer.smtp_relays` now understands `smtp://host:25`
  (opportunistic STARTTLS), `smtp://host:587` (STARTTLS **enforced** —
  soft failure instead of a plaintext fallback), `smtps://host:465`
  (implicit TLS) and `smtp://user:pass@host:587` (AUTH PLAIN after the
  TLS handshake, userinfo percent-decoded) — the smarthost path when the
  provider blocks outbound port 25.

## [0.1.0] - 2026-07-11

The first CamelMailer release — a transactional email platform in one
Rust binary and one PostgreSQL database. CamelMailer began as a
ground-up Rust rewrite of [Postal](https://github.com/postalserver/postal)
(MIT) and is an independent project.

### Added

- **SMTP server** — full protocol state machine (PROXY protocol,
  HELO/EHLO, STARTTLS via rustls, AUTH PLAIN/LOGIN/CRAM-MD5, all routing
  branches, dot-unstuffing, size limits, loop detection, From/Sender
  domain authentication).
- **Delivery worker** — `SKIP LOCKED` queue, MX/relay delivery with
  opportunistic outbound STARTTLS, IP-pool source addresses, exponential
  backoff, suppression holds, DKIM signing (RFC 6376), open/click
  tracking rewrite, rspamd/ClamAV inspection (opt-in), HTTP route
  delivery, webhook queue with retries and RSA signing.
- **HTTP APIs** (74 endpoints, one OpenAPI 3.0 spec, stable
  `{ status, time, data | error }` envelope):
  - Messaging (`/api/v2/server`, `X-Server-API-Key`): send raw/templated,
    single/batch; messages, deliveries, opens, clicks, raw source; stats,
    bounces, streams, inbound with bypass/retry; templates with a safe
    Mustache-subset renderer and dry-run preview.
  - Management (`/api/v2/admin`, `X-Admin-API-Key` or Bearer):
    organizations, servers, domains, credentials, routes, webhooks,
    suppressions, users, IP pools, admin API keys, auth audit log.
  - Accounts (`/api/v2/auth`): login with lockout and TOTP 2FA,
    self-registration (`auth.allow_registration`), password resets,
    invitations, OIDC single sign-on (code flow + PKCE); RBAC
    (viewer/member/admin/owner per organization, plus global admins).
- **Platform mail (dogfooding)** — password-reset, invitation and
  welcome mails are sent through the installation's own pipeline via a
  configurable tenant credential (`app_mail` config group).
- **Tenant isolation in the database** — one PostgreSQL database;
  row-level security with `FORCE ROW LEVEL SECURITY` on message data,
  enforced per-transaction via `set_config('camelmailer.server_id', …)`.
- **Web dashboard** — Next.js app (shadcn/ui): login/2FA/SSO,
  registration, organizations and roles, servers with all resources,
  sending and message browsing.
- **Template library** — 20 ready-to-clone transactional templates with
  a one-command import script.
- **Install paths** — from source (Docker Compose), prebuilt multi-arch
  images on GHCR with a single-file compose, and `.deb` packages
  (amd64/arm64) with systemd units.
- **Postal compatibility** — existing `postal.yml` config files load
  unchanged (`postal:` group alias, `POSTAL_CONFIG_FILE_PATH`).

[Unreleased]: https://github.com/camelmailer/camelmailer/compare/v0.4.0...HEAD
[0.4.0]: https://github.com/camelmailer/camelmailer/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/camelmailer/camelmailer/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/camelmailer/camelmailer/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/camelmailer/camelmailer/releases/tag/v0.1.0
