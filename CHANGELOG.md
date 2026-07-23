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

## [0.7.6] - 2026-07-23

### Added

- **Per-domain SPF opt-out**, mirroring the DMARC opt-out (0.7.5). A domain
  whose SPF is managed externally — an existing multi-provider record the
  owner will not extend with our `include:` — can disable its SPF health
  check via `PATCH …/domains/{name}` with `{"check_spf": false}` (new
  `check_spf` field, default `true`, migration `0042`). The check then
  reports SPF as `ignored` (no problems, excluded from the `overall` grade
  and from the DMARC "fix SPF/DKIM" next-step). `PATCH` now accepts
  `check_dmarc` and/or `check_spf` (at least one required);
  `AdminStore::set_domain_spf_check` on both stores.

## [0.7.5] - 2026-07-23

### Added

- **Per-domain DMARC opt-out.** A sending domain can disable its DMARC
  health check via `PATCH …/domains/{name}` with `{"check_dmarc": false}`
  (new `check_dmarc` field on the domain, default `true`, migration `0041`).
  When disabled the domain health check reports the DMARC row as `ignored`
  (no problems, and it no longer affects the `overall` grade) — for domains
  whose DNS and DMARC policy are managed externally, where a "missing"
  DMARC record is noise. `AdminStore` gained `set_domain_dmarc_check`
  (both `MemoryStore` and `PgStore`); the health `overall` ranks `ignored`
  like `ok`.

## [0.7.4] - 2026-07-23

### Added

- **Brandable domain-verification challenge.** The ownership-challenge TXT
  record was hardcoded to `_camelmailer-challenge.<domain>` /
  `camelmailer-verification=<token>`. Two new `dns` settings —
  `verification_record_label` (default `_camelmailer-challenge`) and
  `verification_value_prefix` (default `camelmailer-verification`) — let an
  installation rebrand it (e.g. `_revenexxmailer-challenge` /
  `revenexx-verification`). Applied to both sending-domain and org-SSO-domain
  verification, in the record the API returns and in the DNS lookup it checks.

### Changed

- **SPF health proposes a merged record when one already exists.** When a
  sending domain already publishes an SPF record that does not include this
  installation, the domain health check now returns the domain's own record
  *extended* with our mechanism (inserted before the `all` term) as the
  value to publish, instead of a standalone `v=spf1 … ~all` that would create
  a second, invalid `v=spf1` record. New helper
  `camelmailer_core::dmarc::spf_with_mechanism`. Domains with no SPF record
  still get the standalone proposal.

## [0.7.3] - 2026-07-23

### Fixed

- **Credentials addressable by uuid.** `GET/PATCH/DELETE
  …/credentials/{id}` also accepts the credential's uuid — Postal's Admin
  API v2 addressed credentials by uuid, so integrations built against it
  (provisioners fetching an SMTP secret by the uuid they stored) received
  `400 Invalid URL … to a u64` instead of the credential. Unknown uuids
  answer 404 like unknown ids.

## [0.7.2] - 2026-07-22

### Fixed

- **IP-pool deliveries to dual-stack MX hosts.** With an IP-pool source
  address bound (IPv4), the SMTP client connected to the first resolved
  address of the MX host — when that was an AAAA record (Microsoft 365,
  Google resolve IPv6-first) the connect failed with `Address family not
  supported` and the delivery soft-failed forever. The client now filters
  the resolved addresses to the bound source's family and reports a clear
  error when the host has no address in that family.

## [0.7.1] - 2026-07-22

### Added

- **Published web dashboard image.** The release pipeline now builds and
  pushes `ghcr.io/camelmailer/camelmailer-web` (multi-arch, same tags as the
  engine image) from the new `web/app/Dockerfile`. The Next server inside
  proxies `/api` and `/health` to a compose service named `web` on port 5000
  (override at build time with `--build-arg API_PROXY_URL=…`), so the app is
  same-origin and needs no CORS setup. `install/docker-compose.yml` now
  starts the dashboard on `:3000` (`DASHBOARD_PORT` to change it) — the
  prebuilt install is no longer headless.

## [0.7.0] - 2026-07-22

### Security

- **Admin API keys are now stored hashed (SHA-256), never in cleartext.**
  `admin_api_keys` previously kept the full secret in the `key` column and
  compared it literally, so a database read yielded working full-access keys.
  Keys are now stored as the SHA-256 hash of the token (mirroring session,
  invitation and reset tokens) plus a short non-secret display prefix; the
  plaintext is generated at creation, returned/printed exactly once (the
  `POST /api/v2/admin/admin_api_keys` response and the `make-admin-api-key`
  CLI), and never persisted. Presented `X-Admin-API-Key` values are hashed
  before lookup. Migration `0038` replaces the `key` column with `key_hash` +
  `key_prefix`; as there are no real keys yet, existing rows are truncated and
  operators regenerate their keys. Both `MemoryStore` and `PgStore` were
  updated in lockstep.
- **SSRF guard on outbound webhook and HTTP-route-endpoint requests.** The
  delivery worker now resolves the destination host before POSTing to a
  tenant-controlled webhook URL (`camelmailer-worker` `process_next_webhook`)
  or HTTP route endpoint (`process_incoming`) and refuses the request when the
  host is, or resolves to, a loopback, private (RFC1918), link-local,
  unique-local (`fc00::/7`), IPv4-mapped/compatible IPv6, or otherwise
  non-global address. This closes a semi-blind SSRF read primitive (webhook
  reply bodies are stored in the tenant-visible audit log) — for example a
  webhook pointed at `http://169.254.169.254/` cloud metadata. The guard is on
  by default and configurable via the new `camelmailer.outbound_ssrf_protection`
  (bool, default true) and `camelmailer.outbound_allowed_hosts` (explicit
  allowlist) settings. Mirrors Postal's v3.3.7 address-guard fix.
- **Explicit request-body size limit on the message-send routes.** The HTTP
  send handlers (`POST /api/v2/server/messages`, its `/batch`,
  `/with_template` and `/with_template/batch` siblings) now carry an axum
  `DefaultBodyLimit` sized from `smtp_server.max_message_size` (MB) plus
  base64/JSON overhead, so an oversized body is rejected with `413 Payload Too
  Large` instead of being buffered into memory. Previously these routes had no
  explicit cap.

### Fixed

- **OIDC/SSO provisioning tolerates missing name claims.** An identity
  provider that omits `family_name` (or `given_name`, or the combined `name`)
  no longer provisions an account with an empty name or fails to log in. User
  provisioning now prefers the discrete `given_name`/`family_name` claims,
  falls back to splitting the combined `name`, and finally to the email
  local-part, so the first name is always non-empty (a single-token name still
  yields an empty last name, which is correct). Applies to all three flows
  (`oidc.rs`, `sso.rs`, `org_sso_login.rs`) via a shared helper. Authorize-URL
  scopes were verified to be space-separated (URL-encoded to `%20`), not
  concatenated, and a regression test now pins this. Mirrors Postal's OIDC
  login-failure fixes.
- **Long free-text values no longer risk insert failures.** All free-text
  columns (delivery `output`/`details`, tracking `user_agent`, etc.) are
  Postgres `TEXT` (unbounded), and the public, unauthenticated click/open
  tracking endpoints now additionally truncate the attacker-controlled
  `User-Agent` header to 255 characters before storing it. Guards against the
  Postal "Data too long for column" class of crash.
- **Deleting a domain no longer leaves routes dangling in the in-memory
  store.** `MemoryStore::delete_domain` now nulls the `domain_id` of any route
  that referenced the deleted domain, matching the Postgres
  `routes.domain_id … ON DELETE SET NULL` foreign key — so the two stores stay
  in behavioural lockstep and a referenced route survives its domain's
  deletion (Postal-audit route/endpoint-deletion class).

- **Stale delivery/webhook queue locks are now reclaimed.** A worker that
  crashed mid-delivery previously stranded a message or webhook request as
  "locked" forever, because dequeue only considered `locked_by IS NULL`. The
  dequeue predicate now also picks up rows whose lock is older than
  `camelmailer.queued_message_lock_stale_days` (default 1 day) — the config
  knob that existed but was wired to nothing. Applies to both
  `queued_messages` and `webhook_requests`.
- **Tracking rewrites no longer corrupt signed mail.** `apply_tracking` skips
  click-link rewriting and open-pixel injection for cryptographically signed
  messages (top-level `multipart/signed` S/MIME or PGP/MIME, and inline PGP
  clearsigned bodies), which a rewrite would otherwise invalidate.
- **IDN destination domains are Punycode-encoded before MX resolution.** The
  delivery worker now IDNA/ASCII-encodes the recipient domain before the
  DNS/MX lookup and connection (`camelmailer-worker` `sender.rs`), so mail to a
  Unicode domain such as `münchen.de` is correctly resolved as
  `xn--mnchen-3ya.de` instead of failing to find any MX records. ASCII domains
  are unaffected.
- **Synthesized Message-IDs use the sending domain, not the server hostname.**
  When an outgoing message built via the HTTP send API carries no `Message-ID`,
  one is now synthesized with the From-address domain as its host part
  (`camelmailer-core` `mime::build_message`) instead of leaking the
  installation's `gethostname()` into `<…@host>`. This keeps the Message-ID
  aligned with the sending domain for DKIM/DMARC and threading. An explicit
  Message-ID override is still respected.
- **Click-tracking preserves the exact original URL.** The link rewrite now
  HTML-entity-decodes `href` values (`&amp;` → `&`, numeric `&#38;`/`&#x26;`)
  before storing them (`camelmailer-worker` `tracking::rewrite_links`), so the
  `/track/c/<token>` redirect reconstructs the URL the sender intended rather
  than a literal `a=1&amp;b=2`. URL percent-encoding (`%20`) is left untouched,
  and long URLs are stored unbounded (`TEXT`), so nothing is truncated.

### Added

- **Scoped admin API keys.** Database-backed admin keys can be created with
  an organization scope or, narrower, a single-server scope
  (`POST /api/v2/admin/admin_api_keys` with `organization`/`server`
  permalinks; `make-admin-api-key [name] --org <permalink> --server
  <permalink>`). A scoped key only reaches its subtree — every other path,
  including the organizations index and all global resources, answers 404 so
  existence is never leaked. Inside its subtree a scoped key still counts as
  a machine key (it may `{"force": true}` domain verification). Unscoped
  keys and the global config key keep full access. Migration `0039` adds the
  nullable `organization_id`/`server_id` columns with `ON DELETE CASCADE`,
  so deleting a tenant revokes its keys.
- **Per-server track domains.** A mail server can carry its own click/open
  tracking domains
  (`/api/v2/admin/organizations/{org}/servers/{server}/track_domains`,
  CRUD + `POST …/{id}/verify`): when a server has a verified track domain,
  the delivery worker's link/pixel rewrites and the broadcast
  List-Unsubscribe headers use it (first verified by id) instead of the
  installation-wide `dns.track_domain`, which stays the fallback.
  Verification resolves the domain's CNAME live (it must point at the web
  hostname or the installation track domain; the `DnsResolver` trait gained
  a `cname` lookup) and machine keys may skip the check with
  `{"force": true}`, mirroring sending domains. Migration `0040` adds the
  `track_domains` table (`ON DELETE CASCADE` with the server).
- **Global server resolver.** `GET /api/v2/admin/servers/find/{permalink}`
  finds a server by permalink across all organizations and returns it joined
  with its organization — the one-call tenant-slug resolver for platform
  integrations. 404 when no server matches; 422 when the permalink exists in
  several organizations (ambiguous). Reserved for unscoped machine keys and
  instance admins.
- **Configurable message retention housekeeper.** A new
  `camelmailer.message_retention_days` setting (default `0` = keep forever)
  lets an operator opt in to automatic deletion of old stored messages. When
  set above zero, the worker's hourly housekeeping (`Worker::housekeep`) prunes
  messages older than the window together with their dependent rows
  (deliveries, opens/loads, links, link_clicks, tracking tokens and any queued
  entries), in FK-safe order and per-tenant under row-level security, via the
  new `ServerStore::prune_messages` (implemented on both `MemoryStore` and
  `PgStore`). The default keeps every message, so nothing is deleted unless
  configured.
- **Non-sending historical message import.** A new admin endpoint,
  `POST …/servers/{server}/messages/import`, writes past messages as
  completed records (with their original timestamps, delivery attempts,
  opens and clicks) WITHOUT ever queuing or delivering them, so a migration
  can carry a server's message history over without re-sending a single
  mail. The batch is capped (500 messages and 50 MB of decoded raw per
  request) as a cloud rate/size guard, and per-message failures are reported
  individually rather than failing the whole batch. This backs the
  [camelmailer-migrate](https://github.com/camelmailer/camelmailer-migrate)
  Postal history import.
- **DKIM key import on domain creation.** `POST …/servers/{server}/domains`
  now accepts an optional `dkim_private_key` (PKCS#8 or PKCS#1 PEM). When
  present it is validated and stored as the domain's signing key instead of
  generating a fresh one, so a migration can carry a domain's existing DKIM
  key over unchanged. This backs the
  [camelmailer-migrate](https://github.com/camelmailer/camelmailer-migrate)
  Postal migration tool.
- **Inbound SPF evaluation (record-only, non-blocking).** Received mail is now
  SPF-checked: the SMTP server evaluates the envelope-From (MAIL FROM) domain's
  SPF policy against the connecting client IP and prepends the verdict as a
  `Received-SPF:` header on the stored message
  (pass/fail/softfail/neutral/none/temperror/permerror). It is purely
  informational — the receive path never rejects on the result. The bounded
  evaluator (`camelmailer-core` `spf`) supports `all`, `ip4`, `ip6`, `a`, `mx`,
  `include` and `redirect=` with the RFC 7208 ten-lookup cap (no macro engine),
  behind the reusable `SpfResolver` DNS abstraction; production uses a
  hickory-backed resolver (`camelmailer-smtp` `HickorySpfResolver`) wired in by
  `SmtpServer::run`, and tests drive it with an in-memory stub.

## [0.6.0] - 2026-07-18

### Added

- **Dashboard overhaul.** A wave of dashboard capabilities, all documented
  under `docs/`:
  - **DMARC compliance dashboard.** Aggregate-report ingestion feeds a per
    domain view with a compliance rate plus SPF and DKIM alignment rates,
    sending sources grouped and classified as compliant, forwarded,
    non-compliant or threat, a daily volume series, and a one-click action
    to create the inbound route that ingests reports at
    `internal://dmarc-reports`.
  - **Block-based template and layout editors.** Templates author their body
    from stackable blocks (heading, subheading, text, button, image, list,
    divider, spacer, footer) with Editor, HTML and Plain-Text modes and a
    live layout-wrapped preview. Layouts are reusable shells with a color
    scheme (primary, background, text, font) and an uploaded logo stored in
    Postgres and served from `GET /assets/layouts/{uuid}/logo`; templates
    wrap their body through `{{{ content }}}`.
  - **Tabbed stream and campaign views.** Streams show Dashboard, Messages,
    and (for broadcast) Subscribers with CSV import and Settings. Campaigns
    show Dashboard, Recipients and Messages, and the campaign compose form
    uses the block editor.
  - **`campaign_id` message filter.** `GET /api/v2/server/messages` accepts
    `?campaign_id=<id>`, and every message payload now carries `campaign_id`.
  - **Layout logo endpoints.** `POST /api/v2/server/layouts/{permalink}/logo`
    stores a logo image (up to 1 MB) and returns its served URL; the public
    `GET /assets/layouts/{uuid}/logo` serves it.
  - **Import and export across resource lists.** CSV, JSON and Excel-CSV
    export with a column picker, and CSV import with a downloadable template,
    on Domains, Routes, Webhooks, Suppressions and more (Credentials and
    Recipients export only; credential secrets are never exported).
  - **Cloud pricing preview.** A public-beta cap of 5,000 emails per month,
    a planned Base package (€5 / 5,000 emails per month, cloud, with in-app
    open and click tracking), and an auto-upgrade versus buy-packages choice.
    Pricing launches after the beta.

## [0.5.0] - 2026-07-16

### Added

- **Broadcast (marketing) message streams.** A stream typed `broadcast`
  now enforces the four things marketing mail needs, while `transactional`
  and `inbound` streams keep their existing behavior:
  - **One-click unsubscribe** (RFC 8058): every broadcast message carries a
    `List-Unsubscribe` header (a signed `https://…/track/u/{token}` URL plus
    a `mailto:`) and `List-Unsubscribe-Post: List-Unsubscribe=One-Click`.
    The public endpoint records the opt-out as a suppression scoped to that
    stream only, so a marketing opt-out never blocks transactional mail.
  - **Stream-scoped suppressions**: `suppressions.stream_id` distinguishes
    server-wide suppressions (hard bounces, manual entries) from per-stream
    opt-outs; the send gate blocks an address when a suppression matches
    `stream_id IS NULL` or the message's stream.
  - **CAN-SPAM compliance footer** built from a per-server
    `broadcast_physical_address`, appended to the HTML and text bodies with
    a visible unsubscribe link.
  - **Opt-in subscribers**: a `subscriptions` table keyed by
    `(server_id, stream_id, address)` with a `subscribed | unsubscribed`
    status; broadcast sends to a non-subscribed address are rejected with
    `422`. Manage subscribers (add, import, remove) from the stream view.
  - **Per-stream IP pool** (`message_streams.ip_pool_id`) so broadcast mail
    sends from its own pool and keeps its reputation isolated. The worker
    resolves the stream pool first, then the server pool.
- **Campaigns.** A first-class entity to plan, schedule and send a
  broadcast to a stream's subscribers: draft, schedule for later (an
  in-process scheduler claims due campaigns per tenant), async expansion
  into one message per subscriber tagged with the `campaign_id`,
  per-campaign analytics, and cancel.
- **Automatic complaint ingestion (FBL/ARF)**: abuse reports are parsed
  into `complaint` suppressions and flip the matching subscriber to
  `unsubscribed`.
- **Documentation**: a full set of feature guides under `docs/`
  (sending, message streams, broadcast, campaigns, suppressions, templates,
  sending domains, inbound routing, webhooks, tracking and deliverability),
  plus a documentation index.

### Changed

- **Stream detail** now surfaces the IP pool in the header and edits it in
  the stream dialog, keeps the suppressions link in the header, and moves
  campaigns into their own area.
- **Green "Active" status pills** across the resource tables and detail
  headers, with a shared `statusTone` mapping.
- **Sidebar organization switcher** moved into the sidebar and reworked as
  a lightbox that holds the organizations table (name, role, server count,
  search); the account menu was restructured (My Account, billing portal,
  admin, docs, changelog).
- **Recipients** are now a standalone sidebar view alongside Messaging.

## [0.4.1] - 2026-07-12

### Fixed

- **Auth gate on the app root**: visiting the dashboard root while signed
  in no longer bounces to the login screen. `/` and `/login` now redirect
  an existing session straight to `/dashboard` (auth is client-side, so a
  server redirect couldn't see it).
- **Outbound direct-to-MX delivery no longer soft-fails on
  `invalid peer certificate: UnknownIssuer`.** Direct-MX delivery now uses
  opportunistic TLS *without* certificate verification (like an MTA's
  `smtp_tls_security_level = may`) and falls back to a fresh plaintext
  connection if the STARTTLS handshake fails, instead of forcing verification
  against webpki roots — which failed against virtually every real MX
  (Microsoft/Outlook and many others), stalling mail forever in retry.
  `smtp.openssl_verify_mode` now governs certificate verification for
  **configured relays only** (a smarthost with a known identity): `peer`
  verifies, `none` accepts any; `smtps://` relays keep implicit TLS with
  verification by default.
- **Credentials page crash**: opening a server's API-keys & SMTP view
  (`/orgs/<org>/servers/<server>/credentials`) could throw
  `TypeError: Cannot read properties of undefined (reading 'length')` and
  blank the page when a listed credential arrived without its secret `key`
  or a dependent query had not loaded yet. The credential helpers
  (`maskKey`, `deriveSmtpHost`) and the key/SMTP render sites are now
  defensive: a missing/empty/`null`/`undefined` key masks to `""` instead
  of throwing, `deriveSmtpHost` tolerates unloaded domains and a nullish
  fallback host, and the page renders regardless. Added vitest unit tests
  covering the empty/missing-field edge cases for `maskKey`,
  `deriveSmtpHost` and `relativeTime`.

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

[Unreleased]: https://github.com/camelmailer/camelmailer/compare/v0.6.0...HEAD
[0.6.0]: https://github.com/camelmailer/camelmailer/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/camelmailer/camelmailer/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/camelmailer/camelmailer/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/camelmailer/camelmailer/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/camelmailer/camelmailer/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/camelmailer/camelmailer/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/camelmailer/camelmailer/releases/tag/v0.1.0
