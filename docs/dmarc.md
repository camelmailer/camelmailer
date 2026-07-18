# DMARC monitoring

CamelMailer can watch a sending domain's authentication health and act
as the destination for the domain's DMARC **aggregate reports** (RUA) —
the loop that tells you whether your mail passes SPF/DKIM alignment and
who else is sending as your domain. Three pieces work together:

1. a **domain health check** — a live DNS check of SPF, DKIM and DMARC
   with a traffic light per record and a recommended next step,
2. **report ingestion** — an inbound route whose target is the internal
   endpoint `internal://dmarc-reports`; mail arriving there is parsed as
   an RFC 7489 aggregate report and stored per tenant,
3. a **compliance API + dashboard tab** — pass rate, top sending
   sources and the stored reports.

## 1. Check a domain's health

```
GET /api/v2/admin/organizations/{org}/servers/{server}/domains/{name}/health
```

(auth like every management endpoint: `X-Admin-API-Key` or a user
session; also available in the dashboard under **Server → DMARC**.)

The endpoint resolves, live via DNS:

| Check | Record | Grading |
|---|---|---|
| SPF | TXT at `<domain>` | `ok` when exactly one `v=spf1` record exists, it includes this installation (`include:<dns.spf_include>`, or `a:<smtp_hostname>` when no include is configured) and ends in `-all`/`~all`; `warning` on multiple records, a missing mechanism or a soft `?all`/`+all`; `missing` without any `v=spf1` record |
| DKIM | TXT at `<dkim_identifier>._domainkey.<domain>` | `ok` when the published `p=` equals the key this server signs with (the domain's own key, or the installation key for older domains); `warning` on a mismatch; `missing` without a record |
| DMARC | TXT at `_dmarc.<domain>` | `ok` when one `v=DMARC1` record with a valid `p=` and an `rua=` exists; `warning` on parse problems, several records or a missing `rua=`; `missing` without a record |

The response carries, per check, `status`, the `found` records, the
`expected` value (copy-paste ready) and the concrete `problems`; plus
the `overall` traffic light (worst of the three), the parsed DMARC
`policy` (`p`, `sp`, `rua`, `pct`), the server's `rua_address` (when an
internal DMARC route exists, see below), the stored `compliance`
figures of the last 30 days and a `next_step` recommendation. DNS
lookup failures grade as `warning` — they are an answer about your
resolver, not about the domain.

### The policy journey

`next_step` walks the usual DMARC rollout:

1. **No DMARC record** → publish
   `v=DMARC1; p=none; rua=mailto:<your RUA address>` and just monitor.
2. **`p=none`** and the reports of the last 30 days show **≥ 10
   messages with ≥ 95 % pass rate** → tighten to `p=quarantine`.
3. **`p=quarantine`** with the same high compliance → consider the
   final step, `p=reject`.

## 2. Receive aggregate reports (RUA)

Reports are ordinary emails, so the receiving side is an ordinary
inbound route — with the special target `internal://dmarc-reports`
instead of an HTTP endpoint (route validation accepts exactly this
value besides `http(s)://` URLs):

```
POST /api/v2/admin/organizations/{org}/servers/{server}/routes
{ "name": "dmarc", "domain": "example.com",
  "endpoint_url": "internal://dmarc-reports" }
```

Then point the `rua=` tag of your DMARC record at that address:

```
_dmarc.example.com.  TXT  "v=DMARC1; p=none; rua=mailto:dmarc@example.com"
```

When a report arrives, the worker does **not** POST it anywhere.
It extracts the report from the message — attachments named `.xml`,
`.xml.gz` (gzip) or `.zip` (first `.xml` entry), or XML directly in the
body; containers are detected by content, not filename — parses it per
RFC 7489 Appendix C and stores it in the tenant's `dmarc_reports` /
`dmarc_report_records` tables. Both tables carry the same FORCE
row-level-security policy as `messages`: rows are only visible inside
the owning server's tenant context. Messages that cannot be parsed as
a report are **held** (visible under the message with a delivery entry
naming the parse error) — a malformed report never crashes the worker.
Successfully ingested messages get the `Processed` status.

Note: many reporters verify the RUA destination for **external**
domains (RFC 7489 §7.1) via a
`<domain>._report._dmarc.<rua-domain>` TXT record. When your RUA
address lives under the same domain as the one being reported on (as
above), no extra record is needed.

## 3. Read the compliance data

All three endpoints authenticate with a server API credential
(`X-Server-API-Key`) and only ever see that server's data:

```
GET /api/v2/server/dmarc/summary?domain=&from=&to=
GET /api/v2/server/dmarc/reports?domain=&from=&to=&page=&per_page=
GET /api/v2/server/dmarc/reports/{id}
```

- **summary** — `total` covered messages, `pass` (DKIM **and** SPF
  aligned), `fail`, `pass_rate`, the top 20 `by_source` entries
  (volume, per-source SPF/DKIM alignment percentages, disposition
  counts) and `by_disposition` totals.
- **reports** — the stored reports, newest range first, paginated
  (`page`/`per_page`, cap 100).
- **reports/{id}** — one report including all its records (source IP,
  count, disposition, raw and aligned SPF/DKIM results, header/envelope
  from).

`from`/`to` select reports whose date range **overlaps** the window.

## 4. The compliance dashboard

The dashboard's **DMARC** tab per server combines all of it. Pick a domain
and the page shows, from top to bottom: the overall traffic light with the
`next_step` hint, the SPF/DKIM/DMARC health cards with a **Re-check**
button that re-runs the live DNS lookup, and, once reports have arrived,
the compliance view built from the stored aggregate records.

The compliance view leads with three rates over the selected window:

- **Compliance rate** is the share of covered volume that is not a threat,
  that is `(volume − threat) / volume`.
- **SPF rate** and **DKIM rate** are the shares of volume that align on SPF
  and on DKIM respectively.

Below the rates, every sending source (a source IP with its resolved
identity) is grouped and classified into one of four categories by how it
aligns:

| Category | Alignment | What it means |
|---|---|---|
| **Compliant** | DKIM and SPF both align | Your own authorized mail. |
| **Forwarded** | DKIM aligns, SPF does not | Legitimate mail that lost SPF in transit (mailing lists, forwarders); DKIM still carries it. |
| **Non-compliant** | SPF aligns, DKIM does not | Authorized by IP but unsigned; usually a sender you still need to set up DKIM for. |
| **Threat / Unknown** | neither aligns | Mail you did not authenticate, including potential spoofing. |

A row of category chips (each with its volume) filters the sources table,
and a daily series charts volume by category over the window so you can
watch compliance climb as you bring sources into alignment.

When no inbound route feeds `internal://dmarc-reports` yet, the tab offers
a **one-click action to create that route** for the selected domain (the
same route described in section 2, `name: "dmarc"`, `mode: "Endpoint"`,
`endpoint_url: "internal://dmarc-reports"`). Publish the matching `rua=`
address and reports start flowing into this view.
