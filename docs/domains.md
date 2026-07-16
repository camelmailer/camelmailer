# Sending domains and authentication

Before a mail server may send as `you@acme.com`, CamelMailer needs to
know that you control `acme.com` and needs the DNS in place so receivers
trust the mail. A **sending domain** ties those together: you add the
domain to a mail server, CamelMailer hands you the DNS records to
publish, you verify ownership, and from then on outgoing mail from that
domain is DKIM-signed and passes SPF.

This page covers adding a domain, the records CamelMailer expects, how
DKIM keys and the verification challenge work, and where to manage all
of it. For the report side of authentication (aggregate reports, the
policy journey from `p=none` to `p=reject`) see
[DMARC monitoring](dmarc.md). For the DNS config group behind these
records see [Configuration](configuration.md).

## What a domain gets when you add it

Adding a domain does two things server-side before you ever touch DNS:

1. It generates a dedicated **RSA-2048 DKIM key** for the domain (a
   PKCS#8 private key, kept in the domain's `dkim_private_key` and never
   returned by the API).
2. It generates a stable **verification token**, published later as a
   TXT record to prove you own the domain.

The response then carries the domain plus the three DNS records to
publish. Each record is `{ name, type, value }`:

| Record | Name | Value | Purpose |
|---|---|---|---|
| Verification | `_camelmailer-challenge.acme.com` | `camelmailer-verification=<token>` | Proves you control the domain |
| SPF | `acme.com` | `v=spf1 include:spf.example.com ~all` | Authorizes this installation to send for the domain |
| DKIM | `camelmailer._domainkey.acme.com` | `v=DKIM1; k=rsa; p=<base64 public key>` | Lets receivers verify the signature the worker adds |

All three are TXT records. The SPF mechanism and the DKIM selector come
from the `dns` config group (see [SPF](#spf) and [DKIM](#dkim) below), so
the exact `name`/`value` you get reflects your installation's
configuration. The DKIM record is `null` in the rare case that neither
the domain nor the installation has a signing key.

Two records CamelMailer does **not** generate for you, on purpose:

- **DMARC.** You publish `_dmarc.acme.com` yourself once SPF and DKIM
  pass. The health check and the [DMARC monitoring](dmarc.md) flow walk
  you through the policy journey and the `rua=` reporting address.
- **A per-domain Return-Path record.** Bounces are received on the
  installation's shared return-path domain. See
  [Return-Path and bounces](#return-path-and-bounces).

## DKIM

Every domain added through the current API carries **its own** DKIM key,
so a leaked or rotated key on one domain never touches another. The key
is generated at creation time (RSA-2048, PKCS#8 PEM) and stored on the
domain. The private half is never serialized; the API and dashboard only
ever show the public half.

### The public record

The `p=` value in the DKIM TXT record is `base64(SubjectPublicKeyInfo
DER)` of the domain's key. CamelMailer derives it on the fly from the
stored private key each time it renders the record, so the value you see
in the API and dashboard is always the current key. The record name uses
the selector from `dns.dkim_identifier` (default `postal`; set it to
something like `camelmailer` and the record becomes
`camelmailer._domainkey.acme.com`).

### Which key signs

Signing happens at **delivery time** in the worker, once the final body
is assembled (after the click-tracking rewrites and the open pixel), so
the signature covers exactly what the recipient receives. The stored copy
of the message stays unsigned.

Key selection for a domain follows one rule:

1. If the domain has its own `dkim_private_key`, that key signs.
2. Otherwise the **installation key** signs (the public half of
   `camelmailer.signing_key_path`). This is the fallback for domains
   created before per-domain keys existed, whose `dkim_private_key` is
   `None`. The fallback stays valid forever.
3. If a per-domain key is present but fails to parse, the worker logs a
   warning and falls back to the installation key, so a bad key never
   leaves mail unsigned.

The selector is the same in every case (`dns.dkim_identifier`), which is
why the health check compares the published `p=` against exactly the key
this server would sign with. When neither a domain key nor an
installation key exists, outgoing mail simply goes out unsigned and the
dashboard shows a "No DKIM key" notice.

A message is only DKIM-signed when it carries an **authenticated
domain**. Mail authorized only by a confirmed single sender address has
no domain to attach, so it goes out unsigned on that count. The signature
itself is RFC 6376 `rsa-sha256`,
relaxed/relaxed canonicalization, over the `From`, `Sender`, `Reply-To`,
`To`, `Cc`, `Subject`, `Date` and `Message-ID` headers that are present.

## DNS-based domain verification

A domain starts `verified: false`. Verification proves you control the
DNS by looking for the challenge token:

```
POST /api/v2/admin/organizations/{org}/servers/{server}/domains/{name}/verify
```

CamelMailer resolves the TXT records at `_camelmailer-challenge.<domain>`
and marks the domain verified when one of them equals
`camelmailer-verification=<token>`. When the record is missing or the
lookup fails, the call returns `422 ValidationError` whose message names
the exact record to publish, so you can copy it straight from the error:

```json
{
  "status": "error",
  "error": {
    "code": "ValidationError",
    "message": "Domain ownership is not proven yet: publish a TXT record at _camelmailer-challenge.acme.com with the value \"camelmailer-verification=Xa9…\", wait for DNS to propagate, then retry"
  }
}
```

Operators driving the API with the `X-Admin-API-Key` machine key have an
escape hatch: sending `{"force": true}` marks the domain verified
without the DNS check. This is deliberately limited to the machine key;
a user session that sends `force` gets `403 Forbidden`.

Verification is what unlocks sending. On every send, the From domain
must either match a **verified** sending domain of the server (or its
org), or the exact From address must be a confirmed sender address.
Only a verified domain attaches a domain id to the message, which is in
turn what triggers DKIM signing for it.

## SPF

The SPF record authorizes this installation's sending infrastructure for
your domain. CamelMailer builds the expected value from config:

- With `dns.spf_include` set (the usual case), the mechanism is
  `include:<dns.spf_include>`, for example
  `v=spf1 include:spf.example.com ~all`.
- With `dns.spf_include` empty, it falls back to the installation's SMTP
  hostname: `v=spf1 a:<camelmailer.smtp_hostname> ~all`.

Publish exactly **one** `v=spf1` record on the domain and keep the `all`
qualifier at `~all` (softfail) or `-all` (hardfail). The health check
grades SPF `ok` when a single `v=spf1` record exists, includes this
installation, and ends in `~all` or `-all`. It warns on a soft `?all`,
an open `+all`, a missing mechanism, or more than one SPF record
(receivers treat multiple `v=spf1` records as a permanent error).

If the domain already sends through another provider, keep it to one
record by merging the mechanisms, for example
`v=spf1 include:spf.example.com include:_spf.google.com ~all`.

## Return-Path and bounces

On the HTTP send path the envelope sender (`MAIL FROM`) is the message's
From address, so bounces flow back toward the sending domain. CamelMailer
receives them on the installation's shared **return-path domain**
(`dns.return_path_domain`, for example `rp.example.com`): the SMTP intake
recognizes a recipient as a return path when its domain is that
return-path domain or begins with the custom return-path prefix
(`dns.custom_return_path_prefix`, default `psrp`), then matches it to the
originating server by token and processes the DSN.

The per-domain records above do not include a return-path entry, because
the return-path domain is installation-wide and shared across every
sending domain. If you want a domain's bounces to travel under its own
subdomain,
publish a CNAME from `psrp.<domain>` to the installation's return-path
domain as an operator-level step. This is optional; bounce classification
works either way.

## Managing domains

### Dashboard

Under **Server → Domains**, opening a domain lands on the detail view
built around the DNS flow:

- A **Records** tab groups the records by purpose (Verification, then
  Sending for SPF and DKIM), with monospace values you can copy and a
  status pill per record fed by the live health check.
- A **Verify** action runs the check; if it is not satisfied yet, the
  view surfaces the exact record you still need to publish.
- A **Health** tab shows SPF, DKIM and DMARC traffic lights with a
  "Re-check DNS" button (the same check described in
  [DMARC monitoring](dmarc.md)).
- An **email the records to a teammate** action opens a prefilled draft
  with every record as plain text, for when someone else owns the DNS.

When a domain has no DKIM key at all (neither its own nor an
installation key), the Sending section replaces the DKIM row with a
notice that outgoing mail will not be signed.

### Admin API

All endpoints authenticate with `X-Admin-API-Key` (machine, full
access) or a user session `Authorization: Bearer` (RBAC-scoped), under
`/api/v2/admin/organizations/{org}/servers/{server}`:

| Method | Path | Does |
|---|---|---|
| `GET` | `/domains` | List domains, each with its three records |
| `POST` | `/domains` | Add a domain (generates the DKIM key + token) |
| `GET` | `/domains/{name}` | Show one domain and its records |
| `DELETE` | `/domains/{name}` | Remove a domain |
| `POST` | `/domains/{name}/verify` | Verify via the DNS challenge (`force` for the machine key) |
| `GET` | `/domains/{name}/health` | Live SPF/DKIM/DMARC health check |

### Example: add and verify a domain

Add the domain:

```bash
curl -X POST \
  -H "X-Admin-API-Key: $ADMIN_KEY" \
  -H "Content-Type: application/json" \
  -d '{"name": "acme.com"}' \
  https://mail.example.com/api/v2/admin/organizations/acme/servers/transactional/domains
```

The response carries the records to publish:

```json
{
  "status": "success",
  "data": {
    "domain": {
      "name": "acme.com",
      "verified": false,
      "verification_record": {
        "name": "_camelmailer-challenge.acme.com",
        "type": "TXT",
        "value": "camelmailer-verification=Xa9kQ2…"
      },
      "spf_record": {
        "name": "acme.com",
        "type": "TXT",
        "value": "v=spf1 include:spf.example.com ~all"
      },
      "dkim_record": {
        "name": "camelmailer._domainkey.acme.com",
        "type": "TXT",
        "value": "v=DKIM1; k=rsa; p=MIIBIjANBgkqhkiG9w0BAQ…"
      }
    }
  }
}
```

Publish those as TXT records at your DNS provider:

```
_camelmailer-challenge.acme.com.  TXT  "camelmailer-verification=Xa9kQ2…"
acme.com.                         TXT  "v=spf1 include:spf.example.com ~all"
camelmailer._domainkey.acme.com.  TXT  "v=DKIM1; k=rsa; p=MIIBIjANBgkqhkiG9w0BAQ…"
```

Wait for propagation, then verify:

```bash
curl -X POST \
  -H "X-Admin-API-Key: $ADMIN_KEY" \
  https://mail.example.com/api/v2/admin/organizations/acme/servers/transactional/domains/acme.com/verify
```

On success the domain comes back `verified: true` and is ready to send.
Add a DMARC record next and watch the health turn green: see
[DMARC monitoring](dmarc.md).
