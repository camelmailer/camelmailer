# Data retention

Camelmailer keeps what it stores until a retention period says otherwise.
The installation sets defaults in the `retention` config group, an
organization can override them, and the worker applies them once an hour.

## Data classes

| Class | What it covers | Config key | Organization override |
|---|---|---|---|
| Message content | The raw MIME of every stored message, outgoing and inbound | `retention.message_content_days` | yes |
| Message metadata | Message rows (status, subject, recipients, tag, metadata), deliveries, opens and clicks with their IP address and user agent, links, tracking tokens, queue entries and share links of the message | `retention.message_metadata_days` | yes |
| Webhook log | The per-server webhook delivery log, including response bodies | `retention.webhook_log_days` | yes |
| DMARC reports | Received aggregate reports and their rows | `retention.dmarc_report_days` | yes |
| Authentication log | Sign-in, 2FA, SSO and SCIM events with IP address and user agent | `retention.auth_event_days` | no |
| API request log | Method, path, status and user agent of server API calls | `retention.api_request_days` (default 30) | no |
| Expired tokens | Sessions, password resets, invitations, message share links, OIDC, passkey and SAML ceremony state | always, at expiry | no |

A value of `0` keeps the data forever. That is the default for every class
except the API request log, so an installation that configures nothing keeps
all of its mail data.

**Message content goes first.** When content expires, the worker empties
the stored MIME and records `content_purged_at`; the message stays visible
with its status, recipients and delivery history until the metadata period
ends. Content cannot outlive its row, so a content period longer than the
metadata period is shortened to it. Messages still waiting for delivery
keep their content.

**What is never removed by retention:** suppressions (they stay while the
server exists, since dropping them would resume mail to people who
unsubscribed or bounced), subscribers, templates and all configuration, and
the monthly usage counters. Deleting a server or an organization removes its
data immediately.

**Deleting a user** keeps their authentication events for the audit trail
and removes the email address and the user reference from them.

## Organization overrides

An installation administrator sets per-organization periods through the
admin API, in the same call as the monthly send limit:

```bash
curl -X PATCH -H "X-Admin-API-Key: $KEY" -H 'content-type: application/json' \
  -d '{"retention": {"message_content_days": 30, "message_metadata_days": 365}}' \
  https://mail.example.com/api/v2/admin/organizations/acme/limits
```

Per field: absent leaves it unchanged, `null` inherits the installation
default, `0` keeps forever, a positive number keeps that many days. An
override replaces the installation default for its class in either
direction, so an organization on a longer plan can keep data longer than
the default. There is no per-server retention setting.

`GET /api/v2/admin/organizations/{org}` returns the effective periods
(`null` = kept forever) under `retention`, together with the organization's
own `overrides`; every member of the organization can read it, and the
dashboard shows it on the Usage page.

## How the cleanup runs

The worker's hourly housekeeping applies retention server by server, inside
each server's tenant context, and deletes in batches of
`retention.batch_size` rows (default 1,000), committing after each batch. A
first run on a large backlog therefore proceeds in small steps instead of
one long transaction. A failure in one class is logged and the pass carries
on with the next.
