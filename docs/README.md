# CamelMailer documentation

CamelMailer is a self-hosted or EU-cloud transactional email platform: a
Rust backend (HTTP API, SMTP server, delivery worker) with a Next.js
dashboard. These pages document how the product behaves so you can
integrate against it and run it in production.

New here? Start with the [Quickstart](quickstart.md) to boot the stack and
send your first message in about five minutes, then read
[Sending email](sending.md).

## Getting started

| Page | What it covers |
|---|---|
| [Quickstart](quickstart.md) | Boot the full stack with Docker Compose and send a message over the HTTP API |
| [Installing on Debian / Ubuntu](install-deb.md) | Native `.deb` packages, systemd units, and the config file |
| [Configuration](configuration.md) | The single YAML file: DKIM signing key, DNS, TLS, SMTP relays, and the production checklist |

## Sending mail

| Page | What it covers |
|---|---|
| [Sending email](sending.md) | The send API and SMTP submission: payload fields, attachments, custom headers, and the response envelope |
| [Message streams](streams.md) | The three stream types (transactional, broadcast, inbound), permalinks, per-stream IP pool, and archiving |
| [Templates](templates.md) | The supported Mustache subset, variables, HTML and text pairing, and the bundled template library |
| [Tracking](tracking.md) | Open pixels and click rewriting, the tracking domain, and privacy considerations |

## Marketing and broadcast

| Page | What it covers |
|---|---|
| [Broadcast streams](broadcast.md) | One-click unsubscribe, the CAN-SPAM footer, opt-in subscribers, and reputation isolation |
| [Campaigns](campaigns.md) | The campaign entity, scheduling, async expansion into per-recipient messages, analytics, and cancel |
| [Suppressions and complaints](suppressions.md) | Bounce, unsubscribe, and complaint suppressions; server-wide versus stream-scoped scope; feedback loops (FBL/ARF) |

## Deliverability and authentication

| Page | What it covers |
|---|---|
| [Sending domains](domains.md) | Adding a domain, DKIM (per-domain key with installation fallback), SPF, and DNS verification |
| [DMARC monitoring](dmarc.md) | Domain health checks and DMARC aggregate report ingestion |
| [Deliverability and IP pools](deliverability.md) | IP pools, the stream then server then default resolution order, and warmup guidance |

## Receiving and integrating

| Page | What it covers |
|---|---|
| [Inbound mail and routing](inbound.md) | Inbound streams, routes, and the HTTP endpoint and internal target kinds |
| [Webhooks](webhooks.md) | Event types, the payload envelope, RSA signature verification, and retry semantics |
| [Accounts, RBAC and SSO](authentication.md) | User accounts, two-factor authentication, organization roles, invitations, OIDC and SAML, and SCIM |

## Account and cloud

| Page | What it covers |
|---|---|
| [Cloud pricing and the public beta](pricing.md) | The beta cap, the coming Base package, over-quota options, and where billing lives |
| [Import and export](import-export.md) | CSV and JSON export, and CSV import, across the dashboard resource lists |

## Reference

The full HTTP API is described by the OpenAPI 3.0 spec at
[`web/app/public/openapi.yaml`](../web/app/public/openapi.yaml). Every
response follows the `{ status, time, data | error }` envelope, and list
endpoints paginate with `page` and `per_page`.

## A note on local development

The Docker Compose stack runs the web server, the SMTP server, and the
delivery worker together, so mail flows end to end. If you run only the
web server (for API development), messages are accepted and queued but stay
undelivered until a worker runs, and delivery-time behavior such as DKIM
signing, open and click tracking, and inbound feedback-loop ingestion does
not occur. Each page calls out where this applies.
