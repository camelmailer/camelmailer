# Provider feedback

The large mailbox providers tell senders how their mail is received. Camelmailer
collects three of these sources and shows them on **Administration → Provider
feedback** (`/admin/infrastructure/feedback`) and through
`GET /api/v2/admin/deliverability/feedback`:

| Source | Provider | What it reports | Granularity |
|---|---|---|---|
| Google Postmaster Tools (API v2) | Gmail | User-reported spam rate, SPF/DKIM/DMARC success, outbound TLS, delivery error rates | per sending domain and day |
| Microsoft SNDS | Outlook.com, Hotmail | Filter result (green, yellow, red), complaint rate, trap hits, sample HELO | per sending IP and day |
| Complaint feedback loops | Microsoft (JMRP), Yahoo (CFL), others | Spam complaints as ARF reports | per provider and day |

Every source is optional and switched on by its credentials alone. A source
without credentials is shown as off; it never stops a process from starting.
Secrets never go into the config file: they come from environment variables
or from files (Docker or systemd secrets) that the config file names.

## How the data arrives

A worker fetches Postmaster Tools and SNDS every
`deliverability_feedback.fetch_interval_seconds` (six hours by default; both
providers publish once a day). Only one worker fetches a source at a time. A
failed fetch is retried after five minutes, then ten, twenty and so on, at
most once per interval, and the last error is shown on the page. Each
snapshot is stored for the day the provider's data is about, so a refetch
replaces it. Snapshots are kept for
`deliverability_feedback.snapshot_retention_days` (400).

Feedback-loop reports are ordinary inbound mail. The worker recognises them
by their ARF envelope (RFC 5965), records the complaint as described in
[Suppressions and complaints](suppressions.md#feedback-loops-fbl--arf), and
counts it for the provider that sent the report. The provider is read from
the report's sender, its `Reporting-MTA` and its `User-Agent`.

## Verdicts

Each snapshot gets one verdict, which the page colours and the warm-up brake
reads:

| Source | ok | warn | bad |
|---|---|---|---|
| Postmaster Tools | spam rate below 0.1 % | spam rate from 0.1 %, or any rejects for low IP or domain reputation | spam rate from 0.3 % |
| SNDS | filter result green | filter result yellow, complaint rate from 0.3 %, or trap hits | filter result red |
| Feedback loops | (a count; rated against the IP's volume by the warm-up) | | |

The thresholds come from the providers: Google asks senders to keep the spam
rate in Postmaster Tools below 0.10 % and never to reach 0.30 %
([Email sender guidelines](https://support.google.com/a/answer/81126)). SNDS
calls a filter result green when fewer than 10 % of the IP's messages were
judged spam, yellow between 10 and 90 %, red above 90 %, and names a complaint
rate below 0.3 % as a good bar
([SNDS FAQ](https://substrate.office.com/ip-domain-management-snds/SNDS/FAQ)).

Google retired domain and IP reputation: the Postmaster Tools API v2 carries
every v1 metric except those two
([announcement](https://support.google.com/mail/answer/16594218)). Camelmailer
reads v2 only, so there is no "reputation HIGH/LOW" column for Gmail; the
spam rate and the reputation-related reject reasons take its place.

## Setting up Google Postmaster Tools

1. Sign in to [Postmaster Tools](https://postmaster.google.com) with the
   Google account that will read the data, and add each sending domain
   (the domain in your `From:` and DKIM `d=`). Verify it with the TXT record
   Postmaster Tools shows. Data appears once a domain sends enough mail to
   Gmail each day; low volumes show nothing.
2. In the [Google Cloud console](https://console.cloud.google.com), create a
   project, enable the **Gmail Postmaster Tools API**, configure the OAuth
   consent screen and create an OAuth client ID of type **Desktop app**
   ([quickstart](https://developers.google.com/workspace/gmail/postmaster/quickstart/python)).
3. Obtain a refresh token for that account with the scope
   `https://www.googleapis.com/auth/postmaster.traffic.readonly`, for example
   with the quickstart script or `oauth2l`. Publish the consent screen
   (production status): refresh tokens of an app in testing status expire
   after seven days
   ([Google OAuth 2.0](https://developers.google.com/identity/protocols/oauth2#expiration)).
4. Configure the worker:

   ```yaml
   deliverability_feedback:
     postmaster_tools:
       domains: [example.com]
       client_id: 1234567890-abc.apps.googleusercontent.com
       client_secret_file: /run/secrets/postmaster_client_secret
       refresh_token_file: /run/secrets/postmaster_refresh_token
   ```

   or set `CAMELMAILER_POSTMASTER_CLIENT_ID`,
   `CAMELMAILER_POSTMASTER_CLIENT_SECRET` and
   `CAMELMAILER_POSTMASTER_REFRESH_TOKEN`. Environment variables win over the
   files.

The API is `POST /v2/domains/{domain}/domainStats:query` on
`gmailpostmastertools.googleapis.com`
([reference](https://developers.google.com/workspace/gmail/postmaster/reference/rest)).
Google publishes no request quota for it; one fetch makes one token request
and one query per domain.

## Setting up Microsoft SNDS

1. Sign in to [SNDS](https://substrate.office.com/ip-domain-management-snds/snds)
   with a Microsoft account. The portal moved there in 2026 from
   `sendersupport.olc.protection.outlook.com/snds`.
2. Under **Request Access**, enter each sending IP (or its range). SNDS sends
   an authorization link to an address it finds in the IP's reverse DNS or
   RDAP record (for example `postmaster@` or `abuse@` of the PTR domain, or
   the abuse contact of the hosting provider). Confirm the link. Access no
   longer needs periodic reattestation.
3. Open **Automated Access** (the `SNDS/Auto` page), switch it on and copy the
   data URL. It carries the access key, so treat it as a secret.
4. Configure the worker:

   ```yaml
   deliverability_feedback:
     snds:
       data_url_file: /run/secrets/snds_data_url
   ```

   or set `CAMELMAILER_SNDS_DATA_URL`.

SNDS aggregates a day after midnight Pacific time and keeps 90 days. IPs that
sent fewer than 100 messages to Outlook.com on a day may have no data. If the
URL stops working (SNDS answers with a redirect or a web page), the source
shows an error asking for the current URL from the Automated Access page.

## Setting up the complaint feedback loops

Feedback loops send each complaint as an ARF report by email. Create one
mailbox address on your installation that receives them, for example
`fbl@mail.example.com`: give that domain an MX record pointing at the
installation's inbound hostname and an inbound route on the **sending** mail
server for the address (any route works; the worker recognises the report
before the route is used). The complaint is attributed on the server that
receives it, so use the server whose mail the reports are about.

- **Microsoft JMRP**: in the SNDS portal, open **Junk Mail Reporting Program**,
  add the sending IPs (they must be in your SNDS access list) and enter the
  report address. Reports typically start within 72 hours
  ([Postmaster services](https://substrate.office.com/ip-domain-management-snds/Postmaster/Services)).
- **Yahoo CFL**: at [Yahoo Sender Hub](https://senders.yahooinc.com/complaint-feedback-loop/),
  create a sender profile, add and verify each sending domain, and enroll it
  with the report address. CFL is domain based and only covers DKIM-signed
  mail: it matches on the DKIM `d=` domain, so every sending domain needs its
  DKIM key published.
- **Gmail** sends no individual reports. Its feedback loop is aggregated and
  appears as the spam rate in Postmaster Tools.

### Suppressing complainants

A complaint about broadcast mail always suppresses the recipient for that
stream. A complaint about transactional mail suppresses the recipient for
the stream of the complained-about message too, by default: someone who
marks your mail as spam should not receive more of it, and providers judge a
sender by whether it keeps mailing complainers.

Each mail server can choose otherwise under **Settings → Spam complaints**
(or `PATCH …/servers/{server}/complaint_settings` with
`{"suppress_complainants": false}`), for example a server that only sends
password resets and invoices, which recipients still need. Then the complaint
is recorded and `MessageComplained` fires, and the sender decides. The
installation default is:

```yaml
deliverability_feedback:
  fbl:
    suppress_complainants: true      # the default; a server's own choice wins
```

Complaint suppressions appear on the server's **Suppressions** page with the
type `complaint` and the reason "Spam complaint (feedback loop)". Filter by
type to see them; **Reactivate** removes one, after which the server mails
the address again.

## Privacy of complaint reports

An ARF report contains the complaining recipient's address and usually the
headers or the full text of the original message. Camelmailer handles it as
follows:

- The report is stored as an inbound message of the server that received it.
  Its raw content (the address and the quoted original) is removed after
  `retention.feedback_report_days` (30 by default; `0` keeps it as long as
  other message content), or earlier when `retention.message_content_days`
  is shorter. `retention.message_metadata_days` removes the message row
  ([Retention](retention.md)).
- The complaint itself becomes a `MessageComplained` webhook to the sender
  and a suppression (always for broadcast mail; for transactional mail unless
  the server switched it off). A suppression keeps the address, which is its
  purpose: the address is never mailed again on that stream. Removing the
  suppression in the dashboard removes the address.
- The provider counts on the Provider feedback page hold numbers only, never
  an address.
- Microsoft and Yahoo may redact the recipient in their reports. Camelmailer
  then attributes the complaint through the message token or `Message-ID` of
  the quoted original.

Tell your recipients in your privacy policy that spam complaints are
processed to stop further mail to them.

## Use by the warm-up

The verdicts are stored so that the adaptive warm-up can read the newest
signals of one IP and provider group in a single query
(`FeedbackStore::latest_provider_signals`): SNDS rows of that IP, plus the
domain-wide Postmaster Tools rows and the provider-wide feedback-loop counts
of that provider. Connecting them to the warm-up brake is a separate change;
until then the signals are shown and change nothing about delivery.
