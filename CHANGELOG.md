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

[Unreleased]: https://github.com/camelmailer/camelmailer/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/camelmailer/camelmailer/releases/tag/v0.1.0
