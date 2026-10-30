# Usage and billing

Every organization has a **Usage** page in the dashboard. It shows how many
messages the organization sent in the current calendar month (UTC), across
all of its servers, and, when the installation sets one, the monthly limit
and what is left of it. The figure comes from the server's own monthly
counter (`GET /api/v2/admin/organizations/{org}/usage`), so it covers every
server whether or not it has an API credential.

## Limits

An installation administrator can give an organization a **monthly send
limit**. At 100 % sending stops: the HTTP API answers `429` with the code
`OrganizationQuotaExceeded`, SMTP answers `550 5.7.1`, and nothing is
stored. The limit resets on the first day of the next month. A per-server
send limit (trailing 30 days) can apply as well, and the stricter of the two
decides. See [Administration](administration.md#organization-quota-and-suspension)
and [Send limits](sending.md#send-limits).

A self-hosted installation has no limit on any organization or server unless
its own operator sets one.

## Billing

The open-source build has no payment integration. Billing appears only on an
installation built with a billing provider (see
[Configuration](configuration.md#billing-billing-provider-custom-builds)).
There, organization admins and owners see the provider's plans, checkout and
subscription management on the same page, rendered by the provider's own web
components, with a plain link as fallback.

The hosted Camelmailer cloud is such a build. Its plans and prices are
published on camelmailer.com.

## Self-hosted installations

A self-hosted install is never charged and shows no billing section.
`GET …/billing` reports `enabled: false` and the other billing endpoints
answer `403 BillingDisabled`. The usage figure renders all the same, so you
can watch your own sending volume.
