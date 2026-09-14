# Security policy

Camelmailer carries other people's mail. A vulnerability here can reach
message content, sending credentials or a whole tenant's data, so reports
are welcome and get priority over feature work.

## Reporting a vulnerability

Two private channels, both reaching the maintainers:

- **Email [security@camelmailer.com](mailto:security@camelmailer.com)**
- **GitHub:** [report a vulnerability](https://github.com/camelmailer/camelmailer/security/advisories/new)
  privately from the Security tab

Please keep it out of public issues, pull requests and Discord until a fix
is released.

Useful in a report, as far as you have it:

- The version or commit, and whether it is self-hosted or the cloud
- Which surface is affected: the SMTP server, the delivery worker, one of
  the HTTP API surfaces (`/api/v2/server`, `/api/v2/admin`,
  `/api/v2/auth`), the SCIM surface, the dashboard, or the configuration
- What an attacker can reach, and what access they need to start
- The smallest reproduction you can manage, such as a request, a message,
  a config snippet, or a curl line

## What to expect

We aim to acknowledge a report within three working days and to agree a
disclosure date with you once the impact is clear. Reporters are credited
in the release notes unless they prefer otherwise. If a report turns out to
describe intended behaviour, you get the reasoning rather than silence.

## Supported versions

Camelmailer is pre-1.0, and fixes land in the next release cut from `main`.
Only the **latest release** carries security fixes, so an installation
staying current stays covered. Releases are listed on the
[releases page](https://github.com/camelmailer/camelmailer/releases) and in
[CHANGELOG.md](CHANGELOG.md).

## Testing

Run your own instance and test against that. It runs the same code as the
hosted cloud, and a self-hosted installation is a few minutes of
`docker compose` (see the [quickstart](docs/quickstart.md)). For permission
to test against the cloud, ask first at
[cloud@camelmailer.com](mailto:cloud@camelmailer.com).

## Working as intended

These come up in reports and are deliberate, so they are documented here
rather than fixed:

- **`camelmailer.admin_api_key`** is a full-access machine credential by
  design, a fallback for bootstrapping. Database-backed keys from
  `camelmailer make-admin-api-key <name>` are the ones meant for daily use,
  because they can be listed and revoked.
- **Running without PostgreSQL** falls back to non-persistent in-memory
  storage and logs a warning at startup. That mode exists for development,
  and a production installation sets `postgres.enabled: true` (or
  `DATABASE_URL`).
- **A request for an organization you are not a member of answers 404**,
  which keeps the existence of other tenants private. The 403 you might
  expect is reserved for resources you can see.

A finding that one of these can be reached in a way the documentation does
not describe is still worth reporting.
