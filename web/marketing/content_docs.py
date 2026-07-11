"""Docs + legal page content for build.py (kept separate for readability)."""

LEGAL_NOTICE = (
    '<div class="notice"><strong>Template.</strong> This document ships with '
    "the open-source project as a starting point. Replace the bracketed "
    "placeholders and have your counsel review it before publishing it for a "
    "commercial offering.</div>"
)


def _legal(title: str, sub: str, body: str) -> str:
    return f'<article><h1>{title}</h1><p class="sub">{sub}</p>{LEGAL_NOTICE}{body}</article>'


DOC_PAGES: dict[str, dict] = {}

# ------------------------------------------------------------- API guide

DOC_PAGES["docs/api.html"] = dict(
    title="API guide — CamelMailer",
    description="Send transactional email over the CamelMailer JSON API: auth, sending, templates, webhooks, errors.",
    active="docs/api.html",
    body="""
    <article>
      <h1>API guide</h1>
      <p class="sub">Everything is JSON over HTTPS. The full contract lives in
      the <a href="../openapi.yaml">OpenAPI specification</a> — import it into
      Postman/Insomnia or generate a typed client.</p>

      <h2>Authentication</h2>
      <table>
        <tr><th>Surface</th><th>Base path</th><th>Header</th></tr>
        <tr><td>Messaging (send, read, templates, stats)</td><td><code class="inline">/api/v2/server</code></td><td><code class="inline">X-Server-API-Key</code></td></tr>
        <tr><td>Management (orgs, servers, domains, …)</td><td><code class="inline">/api/v2/admin</code></td><td><code class="inline">X-Admin-API-Key</code> or <code class="inline">Authorization: Bearer</code></td></tr>
        <tr><td>Accounts (login, 2FA, SSO)</td><td><code class="inline">/api/v2/auth</code></td><td><code class="inline">Authorization: Bearer</code></td></tr>
      </table>
      <p>For application integration you only need one credential: create an
      <strong>API credential</strong> for your mail server in the dashboard
      (Server → Credentials) and put it in
      <code class="inline">X-Server-API-Key</code>.</p>

      <h2>The response envelope</h2>
      <pre>{ "status": "success", "time": 0.004, "data": { … } }
{ "status": "error",   "time": 0.004, "error": { "code": "ValidationError", "message": "…" } }</pre>
      <p>Branch on <code class="inline">error.code</code>, not on prose. The
      codes are stable: <code class="inline">Unauthorized</code>,
      <code class="inline">Forbidden</code>, <code class="inline">NotFound</code>,
      <code class="inline">ValidationError</code>, <code class="inline">ParameterMissing</code>
      — plus the auth-specific ones documented in the spec.</p>

      <h2>Send a message</h2>
      <pre>curl -X POST https://mail.yourdomain.com/api/v2/server/messages \\
  -H "X-Server-API-Key: $KEY" -H "Content-Type: application/json" \\
  -d '{
    "from": { "email": "billing@yourdomain.com", "name": "Acme Billing" },
    "to": ["customer@example.com"],
    "subject": "Your receipt",
    "text_body": "Thanks! You paid €49.00.",
    "html_body": "&lt;p&gt;Thanks! You paid &lt;strong&gt;€49.00&lt;/strong&gt;.&lt;/p&gt;",
    "tag": "receipt",
    "metadata": { "order_id": 1042 }
  }'</pre>
      <p>The <code class="inline">from</code> domain must be a verified sending
      domain of the server. One message is queued per recipient; the response
      carries a token per recipient for later lookups. Attachments go in
      <code class="inline">attachments: [{name, content_type, data_base64}]</code>.</p>

      <h3>Node.js</h3>
      <pre>const res = await fetch("https://mail.yourdomain.com/api/v2/server/messages", {
  method: "POST",
  headers: { "X-Server-API-Key": process.env.CAMELMAILER_KEY,
             "Content-Type": "application/json" },
  body: JSON.stringify({
    from: "billing@yourdomain.com",
    to: [user.email],
    subject: "Your receipt",
    text_body: `Thanks! You paid €${amount}.`,
  }),
})
const { status, data, error } = await res.json()
if (status !== "success") throw new Error(`${error.code}: ${error.message}`)</pre>

      <h3>Python</h3>
      <pre>import requests

r = requests.post(
    "https://mail.yourdomain.com/api/v2/server/messages",
    headers={"X-Server-API-Key": KEY},
    json={"from": "billing@yourdomain.com", "to": [user.email],
          "subject": "Your receipt", "text_body": f"Thanks! You paid €{amount}."},
)
body = r.json()
assert body["status"] == "success", body["error"]</pre>

      <h2>Send with a stored template</h2>
      <pre>curl -X POST https://mail.yourdomain.com/api/v2/server/messages/with_template \\
  -H "X-Server-API-Key: $KEY" -H "Content-Type: application/json" \\
  -d '{
    "from": "hello@yourdomain.com",
    "to": ["ada@example.com"],
    "template": "welcome",
    "template_model": { "name": "Ada", "product": "Acme", "action_url": "https://app.acme.com/start" }
  }'</pre>
      <p>Templates use a safe Mustache subset —
      <code class="inline">{{ variable }}</code>, sections
      <code class="inline">{{#items}}…{{/items}}</code>, inverted sections and
      dotted paths; output is HTML-escaped by default
      (<code class="inline">{{{ raw }}}</code> opts out). Start from the
      <a href="../templates.html">template library</a>, preview with
      <code class="inline">POST /templates/{permalink}/render</code>.</p>

      <h2>Query messages & deliveries</h2>
      <pre># search what you sent
GET /api/v2/server/messages?scope=outgoing&query=receipt&per_page=50
# one message + its delivery attempts
GET /api/v2/server/messages/1042
GET /api/v2/server/messages/1042/deliveries
# counters for dashboards
GET /api/v2/server/stats</pre>

      <h2>Webhooks</h2>
      <p>Create a webhook on the server (dashboard or admin API) and receive
      POSTs for message events — deliveries, failures, bounces, opens, clicks.
      Payloads can be RSA-signed with the installation key so you can verify
      origin; failed deliveries are retried with backoff. Details:
      <a href="https://github.com/relationsoftware/postal">docs/ in the repository</a>.</p>

      <h2>SMTP, if you prefer</h2>
      <p>Everything the HTTP API accepts you can also hand over via SMTP
      (port 25/587 of your instance) using an SMTP credential — useful for
      frameworks that already speak SMTP. Same pipeline, same tracking.</p>

      <h2>Rate & size limits</h2>
      <ul>
        <li>Self-hosted: none imposed by the software beyond
        <code class="inline">smtp_server.max_message_size</code>.</li>
        <li>Cloud: message size 14&nbsp;MB; API bursts are throttled per key —
        batch endpoints are the intended path for spikes.</li>
      </ul>
    </article>
""",
)

# ----------------------------------------------------- self-hosting guide

DOC_PAGES["docs/self-hosting.html"] = dict(
    title="Self-hosting guide — CamelMailer",
    description="Run CamelMailer on your own infrastructure: Docker Compose quickstart, DNS, DKIM, production checklist.",
    active="docs/self-hosting.html",
    body="""
    <article>
      <h1>Self-hosting guide</h1>
      <p class="sub">One Rust binary, one PostgreSQL. If you can run Docker
      Compose, you can run your own mail platform.</p>

      <h2>What you need</h2>
      <ul>
        <li>A Linux host with Docker (1 vCPU / 1&nbsp;GB is enough to start).</li>
        <li>A domain, plus DNS control for it.</li>
        <li><strong>Outbound port 25</strong> — many clouds block it by default;
        request unblocking (AWS/Hetzner/OVH all have a form for it) or relay
        through <code class="inline">smtp_relays</code>.</li>
        <li>Reverse DNS (PTR) on your sending IP, matching your SMTP hostname.</li>
      </ul>

      <h2>Boot the stack</h2>
      <pre>git clone https://github.com/relationsoftware/postal && cd postal/camelmailer
cp .env.example .env            # set POSTGRES_PASSWORD
docker compose up -d --build
curl http://localhost:5000/health</pre>
      <p>This starts PostgreSQL, runs migrations, and launches the HTTP API
      (<code class="inline">:5000</code>), the SMTP server
      (<code class="inline">:25</code>) and the delivery worker.</p>

      <h2>First account & first mail</h2>
      <pre># an admin login for the dashboard
docker compose exec web camelmailer make-user you@yourdomain.com Ada Ops --admin

# or script everything with a machine key
docker compose exec web camelmailer make-admin-api-key ops</pre>
      <p>Sign in, create an organization → mail server → sending domain →
      API credential, then send. The
      <a href="api.html">API guide</a> covers the calls; the dashboard does the
      same via clicks.</p>

      <h2>DNS you must set</h2>
      <table>
        <tr><th>Record</th><th>Purpose</th></tr>
        <tr><td><code class="inline">A mail.yourdomain.com</code></td><td>The instance (API + dashboard, TLS via your reverse proxy)</td></tr>
        <tr><td><code class="inline">MX</code> on inbound domains</td><td>Receiving replies/bounces → your instance</td></tr>
        <tr><td><code class="inline">TXT</code> SPF</td><td><code class="inline">v=spf1 a:mail.yourdomain.com -all</code> on sending domains</td></tr>
        <tr><td><code class="inline">TXT</code> DKIM</td><td>Publish the DKIM public key (printed by <code class="inline">make dkim</code> / setup)</td></tr>
        <tr><td><code class="inline">TXT</code> DMARC</td><td><code class="inline">v=DMARC1; p=quarantine; rua=mailto:dmarc@yourdomain.com</code></td></tr>
        <tr><td>PTR (reverse DNS)</td><td>Sending IP → SMTP hostname; set at your hosting provider</td></tr>
      </table>

      <h2>Production checklist</h2>
      <ul>
        <li>Terminate TLS for the API/dashboard at a reverse proxy (Caddy,
        nginx, Traefik) in front of port 5000.</li>
        <li>Enable SMTP STARTTLS: <code class="inline">smtp_server.tls_enabled</code>
        with your certificate.</li>
        <li>Set <code class="inline">auth.frontend_url</code> and
        <code class="inline">web_server.cors_origins</code> if you host the
        dashboard on its own origin.</li>
        <li>Back up PostgreSQL (that's <em>all</em> the state) —
        <code class="inline">pg_dump</code> on a timer is fine to start.</li>
        <li>Warm up fresh sending IPs gradually; keep transactional volume
        steady rather than bursty while reputation builds.</li>
        <li>Upgrades: pull, <code class="inline">docker compose up -d --build</code>
        — migrations run automatically via the one-shot migrate service.</li>
      </ul>

      <h2>Configuration</h2>
      <p>Everything lives in one YAML file
      (<code class="inline">config/camelmailer.example.yml</code> documents all
      groups) or environment variables for the essentials
      (<code class="inline">DATABASE_URL</code>). Accounts, RBAC, SSO and CORS
      are covered in the repository's
      <a href="https://github.com/relationsoftware/postal">docs/authentication.md</a>.</p>

      <h2>Sizing</h2>
      <p>The binary is a few dozen MB of RSS per role; PostgreSQL dominates.
      A 2&nbsp;vCPU / 4&nbsp;GB box comfortably handles hundreds of thousands
      of transactional messages a day. Scale by giving PostgreSQL room first,
      then add worker replicas.</p>
    </article>
""",
)

# ------------------------------------------------------------------ legal

DOC_PAGES["legal/imprint.html"] = dict(
    title="Imprint — CamelMailer",
    description="Legal disclosure / Impressum.",
    body=_legal(
        "Imprint",
        "Legal disclosure (Impressum) for the CamelMailer cloud service.",
        """
      <h2>Service provider</h2>
      <p>[COMPANY LEGAL NAME]<br>
      [STREET ADDRESS]<br>
      [POSTAL CODE, CITY]<br>
      [COUNTRY]</p>
      <p>Commercial register: [REGISTER COURT, REGISTRATION NUMBER]<br>
      VAT ID: [VAT ID]<br>
      Represented by: [MANAGING DIRECTOR(S)]</p>
      <h2>Contact</h2>
      <p>Email: <a href="mailto:hello@camelmailer.example">hello@camelmailer.example</a><br>
      Phone: [PHONE]</p>
      <h2>Responsible for content</h2>
      <p>[NAME, ADDRESS AS ABOVE]</p>
      <h2>Dispute resolution</h2>
      <p>We are neither willing nor obliged to participate in dispute
      resolution proceedings before a consumer arbitration board. The EU
      Commission's platform for online dispute resolution:
      <a href="https://ec.europa.eu/consumers/odr/">ec.europa.eu/consumers/odr</a>.</p>
""",
    ),
)

DOC_PAGES["legal/privacy.html"] = dict(
    title="Privacy policy — CamelMailer",
    description="How the CamelMailer cloud processes personal data.",
    body=_legal(
        "Privacy policy",
        "How the CamelMailer cloud service processes personal data (GDPR).",
        """
      <h2>1. Controller</h2>
      <p>[COMPANY LEGAL NAME], [ADDRESS] ("we"). Data protection contact:
      <a href="mailto:privacy@camelmailer.example">privacy@camelmailer.example</a>.
      This policy covers the <strong>cloud service</strong>; if you self-host
      CamelMailer, you are the controller of your instance and this policy
      does not apply.</p>

      <h2>2. What we process</h2>
      <table>
        <tr><th>Category</th><th>Examples</th><th>Purpose / legal basis</th></tr>
        <tr><td>Account data</td><td>Name, email, password hash, 2FA state, SSO subject</td><td>Providing the service (Art. 6(1)(b) GDPR)</td></tr>
        <tr><td>Customer content</td><td>Messages you send/receive through your servers, templates, suppression lists</td><td>Processed on your behalf as processor (Art. 28; see <a href="dpa.html">DPA</a>)</td></tr>
        <tr><td>Usage & security data</td><td>API logs, auth audit events, IP addresses, user agents</td><td>Security, abuse prevention, billing (Art. 6(1)(f)/(b))</td></tr>
        <tr><td>Billing data</td><td>Company details, invoices, payment references</td><td>Contract & legal obligations (Art. 6(1)(b)/(c))</td></tr>
      </table>

      <h2>3. Where it lives</h2>
      <p>All production systems run on EU infrastructure operated by the
      providers listed on the <a href="sub-processors.html">sub-processors</a>
      page. We do not transfer customer content outside the EU/EEA. Mail you
      send is, by nature, delivered to its recipients' providers worldwide.</p>

      <h2>4. Retention</h2>
      <ul>
        <li>Message content and events: [30/60/90] days by default, then deleted;
        configurable per server.</li>
        <li>Auth audit events: [12] months.</li>
        <li>Account and billing data: for the life of the contract plus
        statutory retention periods.</li>
      </ul>

      <h2>5. Your rights</h2>
      <p>Access, rectification, erasure, restriction, portability and
      objection under Art. 15–21 GDPR, plus complaint to a supervisory
      authority. Write to
      <a href="mailto:privacy@camelmailer.example">privacy@camelmailer.example</a>.</p>

      <h2>6. Cookies</h2>
      <p>The dashboard uses no third-party trackers and no advertising
      cookies. Session state is a bearer token stored in your browser's local
      storage; this website sets no cookies at all.</p>
""",
    ),
)

DOC_PAGES["legal/terms.html"] = dict(
    title="Terms of service — CamelMailer",
    description="Terms for the CamelMailer cloud service.",
    body=_legal(
        "Terms of service",
        "The agreement between you and [COMPANY LEGAL NAME] for the cloud service.",
        """
      <h2>1. Scope</h2>
      <p>These terms govern the managed CamelMailer cloud. The open-source
      software itself is licensed separately under the
      <a href="../open-source.html">MIT license</a>; nothing here restricts
      your rights under it.</p>
      <h2>2. The service</h2>
      <p>We provide hosted transactional email infrastructure: an API,
      dashboard, SMTP endpoints and delivery pipeline according to your plan.
      Beta features are marked as such and may change.</p>
      <h2>3. Your obligations</h2>
      <ul>
        <li>Only send mail recipients expect — see the
        <a href="acceptable-use.html">Acceptable Use Policy</a>, which is part
        of these terms.</li>
        <li>Keep credentials confidential; you are responsible for activity
        under your keys.</li>
        <li>Provide accurate account and billing information.</li>
      </ul>
      <h2>4. Fees</h2>
      <p>Plans are billed [monthly] in advance; overage per 1,000 accepted
      recipients in arrears, as listed on the
      <a href="../pricing.html">pricing page</a> at the time of use. Prices
      exclude VAT.</p>
      <h2>5. Availability</h2>
      <p>We target the uptime stated in your plan; scheduled maintenance is
      announced in advance. Remedies for missed SLAs are service credits as
      described in the plan, and they are the exclusive remedy.</p>
      <h2>6. Data</h2>
      <p>Customer content remains yours. We process it only to provide the
      service, under the <a href="dpa.html">Data Processing Addendum</a>. You
      can export your data and leave at any time — the software is open
      source.</p>
      <h2>7. Suspension & termination</h2>
      <p>Either party may terminate at the end of a billing period. We may
      suspend sending immediately for AUP violations, non-payment, or acute
      deliverability risk, and will notify you.</p>
      <h2>8. Liability</h2>
      <p>Liability is capped at the fees paid in the [12] months preceding the
      claim, and excludes indirect and consequential damages, to the extent
      permitted by law. Mandatory statutory liability remains unaffected.</p>
      <h2>9. Governing law</h2>
      <p>[JURISDICTION]. Place of jurisdiction: [CITY].</p>
""",
    ),
)

DOC_PAGES["legal/dpa.html"] = dict(
    title="Data Processing Addendum — CamelMailer",
    description="Art. 28 GDPR data processing terms for the CamelMailer cloud.",
    body=_legal(
        "Data Processing Addendum",
        "Processing of personal data on your behalf (Art. 28 GDPR).",
        """
      <h2>1. Roles</h2>
      <p>For customer content (messages, recipient data, templates,
      suppression lists) you are the <strong>controller</strong> and
      [COMPANY LEGAL NAME] is the <strong>processor</strong>.</p>
      <h2>2. Subject matter & duration</h2>
      <p>Processing of personal data as necessary to provide transactional
      email delivery, for the duration of the service agreement.</p>
      <h2>3. Nature and purpose</h2>
      <p>Receiving, storing, queueing, delivering and reporting on email
      messages; abuse and deliverability protection.</p>
      <h2>4. Instructions</h2>
      <p>We process customer content only on your documented instructions —
      the API calls and configuration you make are the instructions — unless
      EU or member-state law requires otherwise.</p>
      <h2>5. Confidentiality & security</h2>
      <p>Personnel are bound to confidentiality. Technical measures include
      encryption in transit, tenant isolation enforced at the database layer
      (row-level security), hashed credentials and audit logging. A detailed
      TOM annex is available on request.</p>
      <h2>6. Sub-processors</h2>
      <p>General authorization for the providers listed at
      <a href="sub-processors.html">sub-processors</a>; changes are announced
      [30] days in advance with a right to object.</p>
      <h2>7. Assistance & deletion</h2>
      <p>We assist with data-subject requests and Art. 32–36 obligations, and
      delete or return customer content at contract end (retention windows
      permitting), then delete remaining copies.</p>
      <h2>8. Audits</h2>
      <p>We provide the information reasonably necessary to demonstrate
      Art. 28 compliance and permit audits — for a self-verifiable start,
      remember the processing software itself is open source.</p>
""",
    ),
)

DOC_PAGES["legal/acceptable-use.html"] = dict(
    title="Acceptable use — CamelMailer",
    description="What may and may not be sent through the CamelMailer cloud.",
    body=_legal(
        "Acceptable Use Policy",
        "The short version: send mail your recipients expect.",
        """
      <h2>Allowed</h2>
      <p>Transactional and operational email triggered by a user's action or
      account relationship: receipts, password resets, security alerts,
      shipping updates, invitations, service notifications.</p>
      <h2>Not allowed</h2>
      <ul>
        <li>Unsolicited bulk email of any kind — the cloud is not a campaign
        tool, and purchased or scraped lists are never acceptable.</li>
        <li>Phishing, malware, or content that impersonates other services.</li>
        <li>Content illegal in the EU or in your recipients' jurisdictions.</li>
        <li>Circumventing suppression lists or recipient opt-outs.</li>
        <li>Probing or degrading the platform (test against your own
        self-hosted instance instead).</li>
      </ul>
      <h2>Enforcement</h2>
      <p>Automated systems and human review may hold suspicious mail
      (visible in your dashboard as <em>held</em>). Serious or repeated
      violations lead to suspension under the
      <a href="terms.html">terms</a>. Report abuse to
      <a href="mailto:abuse@camelmailer.example">abuse@camelmailer.example</a>.</p>
""",
    ),
)

DOC_PAGES["legal/sub-processors.html"] = dict(
    title="Sub-processors — CamelMailer",
    description="Infrastructure providers used by the CamelMailer cloud.",
    body=_legal(
        "Sub-processors",
        "Providers that process customer data on our behalf.",
        """
      <table>
        <tr><th>Provider</th><th>Purpose</th><th>Location</th></tr>
        <tr><td>[EU HOSTING PROVIDER, e.g. Hetzner Online GmbH]</td><td>Compute, storage, network</td><td>EU ([COUNTRY])</td></tr>
        <tr><td>[EU BACKUP PROVIDER]</td><td>Encrypted database backups</td><td>EU ([COUNTRY])</td></tr>
        <tr><td>[PAYMENT PROVIDER]</td><td>Payment processing (billing data only, no message content)</td><td>[LOCATION]</td></tr>
      </table>
      <p>Changes are announced [30] days in advance to the billing contact,
      with a right to object under the <a href="dpa.html">DPA</a>. Email
      delivery necessarily involves transmitting messages to your recipients'
      mail providers; those are not sub-processors.</p>
""",
    ),
)
