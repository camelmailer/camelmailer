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

### Added

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

[Unreleased]: https://github.com/camelmailer/camelmailer/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/camelmailer/camelmailer/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/camelmailer/camelmailer/releases/tag/v0.1.0
