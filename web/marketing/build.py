#!/usr/bin/env python3
"""Generate the static marketing pages from a shared layout.

Dependency-free output: every page links style.css and nothing else.
Run `python3 build.py` from this directory after editing PAGES below.
"""

import os

FAVICON = (
    'data:image/svg+xml,<svg xmlns=%22http://www.w3.org/2000/svg%22 '
    'viewBox=%220 0 100 100%22><text y=%22.9em%22 font-size=%2290%22>🐫</text></svg>'
)

NAV = [
    ("pricing.html", "Pricing"),
    ("templates.html", "Templates"),
    ("docs/api.html", "API"),
    ("docs/self-hosting.html", "Self-hosting"),
    ("open-source.html", "Open source"),
    ("/login", "Sign in"),
]

FOOTER_COLS = [
    ("Product", [
        ("pricing.html", "Pricing"),
        ("templates.html", "Template library"),
        ("openapi.yaml", "OpenAPI spec"),
        ("/login", "Sign in"),
    ]),
    ("Developers", [
        ("docs/api.html", "API guide"),
        ("docs/self-hosting.html", "Self-hosting guide"),
        ("open-source.html", "Open source"),
        ("https://github.com/relationsoftware/postal", "GitHub"),
    ]),
    ("Legal", [
        ("legal/imprint.html", "Imprint"),
        ("legal/privacy.html", "Privacy policy"),
        ("legal/terms.html", "Terms of service"),
        ("legal/dpa.html", "Data processing (DPA)"),
        ("legal/acceptable-use.html", "Acceptable use"),
        ("legal/sub-processors.html", "Sub-processors"),
    ]),
]


def rel(depth: int, href: str) -> str:
    if href.startswith(("http", "/", "#", "mailto:")):
        return href
    return "../" * depth + href


def layout(*, title: str, description: str, body: str, depth: int, active: str = "") -> str:
    nav_parts = []
    for href, label in NAV:
        cls = ' class="active"' if href == active else ""
        nav_parts.append(f'<a href="{rel(depth, href)}"{cls}>{label}</a>')
    nav = "".join(nav_parts)
    cols = "".join(
        "<div><h4>{}</h4>{}</div>".format(
            heading,
            "".join(f'<a href="{rel(depth, href)}">{label}</a>' for href, label in links),
        )
        for heading, links in FOOTER_COLS
    )
    return f"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title}</title>
  <meta name="description" content="{description}">
  <link rel="icon" href="{FAVICON}">
  <link rel="stylesheet" href="{rel(depth, 'style.css')}">
</head>
<body>
  <div class="wrap">
    <header class="site">
      <a class="logo" href="{rel(depth, 'index.html')}">🐫 CamelMailer</a>
      <nav class="site">{nav}</nav>
    </header>
{body}
    <footer class="site">
      <div class="cols">{cols}</div>
      <span>© 2026 CamelMailer. MIT licensed. Self-host it or let us run it.</span>
    </footer>
  </div>
</body>
</html>
"""


def legal_page(title: str, sub: str, body: str) -> str:
    notice = (
        '<div class="notice"><strong>Template.</strong> This document is a '
        "starting point shipped with the open-source project. Replace the "
        "bracketed placeholders and have your counsel review it before "
        "publishing it for a commercial offering.</div>"
    )
    return f'<article><h1>{title}</h1><p class="sub">{sub}</p>{notice}{body}</article>'


PAGES: dict[str, dict] = {}

# ------------------------------------------------------------------ index

PAGES["index.html"] = dict(
    title="CamelMailer — transactional email, nothing else",
    description=(
        "The self-hosted or EU-cloud alternative to SendGrid, Mailgun and "
        "Postmark. Simple transactional email over a JSON API. Open source, MIT."
    ),
    active="",
    body="""
    <section class="hero">
      <span class="kicker">Open source · MIT · Self-hosted or EU cloud</span>
      <h1>Transactional email.<br><em>Nothing else.</em></h1>
      <p>
        Receipts, password resets, magic links, alerts — the mail your product
        must send. No marketing suite, no campaign builder, no bloat. A JSON
        API and an SMTP port, backed by one Rust binary and one PostgreSQL.
        Run it yourself, or let us run it for you in the EU.
      </p>
      <div class="cta">
        <a class="btn primary" href="docs/api.html">Send your first mail →</a>
        <a class="btn ghost" href="docs/self-hosting.html">Self-host in 5 minutes</a>
      </div>
      <pre class="code"><span class="c"># That's the whole integration:</span>
curl -X POST https://mail.yourdomain.com/api/v2/server/messages \\
  -H <span class="s">"X-Server-API-Key: $KEY"</span> -H <span class="s">"Content-Type: application/json"</span> \\
  -d '{
    "from": "billing@yourdomain.com",
    "to": ["customer@example.com"],
    "template": "payment-receipt",
    "template_model": { "amount": "49.00", "invoice": "INV-1042" }
  }'</pre>
    </section>

    <section id="why">
      <h2>Why leave the big US providers?</h2>
      <p class="lead">
        SendGrid, Mailgun and friends are fine products — that solve a much
        bigger problem than you have. If all you send is transactional mail,
        you are paying for a marketing platform and mailing your customer
        data across the Atlantic to get it.
      </p>
      <table class="compare">
        <tr><th></th><th>US email SaaS</th><th>CamelMailer</th></tr>
        <tr><td>Focus</td><td class="meh">Marketing suite + transactional add-on</td><td class="yes">Transactional only — by design</td></tr>
        <tr><td>Your data</td><td class="no">Their cloud, their jurisdiction</td><td class="yes">Your servers — or our EU cloud</td></tr>
        <tr><td>Source code</td><td class="no">Closed</td><td class="yes">MIT — audit it, fork it, run it</td></tr>
        <tr><td>Exit path</td><td class="no">Migration project</td><td class="yes">Same code self-hosted; leave anytime</td></tr>
        <tr><td>Pricing model</td><td class="meh">Contacts, seats, feature gates</td><td class="yes">Emails sent. That's it.</td></tr>
        <tr><td>Footprint</td><td class="meh">—</td><td class="yes">One binary + PostgreSQL</td></tr>
      </table>
    </section>

    <section id="features">
      <h2>Everything transactional mail needs. Nothing it doesn't.</h2>
      <div class="grid" style="margin-top: 24px">
        <div class="card"><span class="tag">Send</span><h3>One POST to send</h3>
          <p>JSON in, queued mail out. Attachments, custom headers, tags,
          metadata, reply-to — and batch endpoints when one recipient isn't enough.</p></div>
        <div class="card"><span class="tag">Templates</span><h3>Stored templates</h3>
          <p>Mustache-style <code class="inline">{{ variables }}</code> rendered
          server-side. Start from our library of 20 ready-made transactional
          templates and keep your HTML out of your application code.</p></div>
        <div class="card"><span class="tag">Deliverability</span><h3>DKIM, IP pools, suppressions</h3>
          <p>Signed mail, per-server sending IPs, automatic bounce processing
          and suppression lists — the unglamorous parts, handled.</p></div>
        <div class="card"><span class="tag">Feedback</span><h3>Webhooks & tracking</h3>
          <p>Delivery, bounce, open and click events pushed to your endpoint —
          RSA-signed. Query everything over the API too.</p></div>
        <div class="card"><span class="tag">Inbound</span><h3>Receive replies</h3>
          <p>Route inbound mail to HTTP endpoints. support@ answered by your
          app, not by a second product.</p></div>
        <div class="card"><span class="tag">Team</span><h3>Enterprise accounts</h3>
          <p>Logins with TOTP 2FA, organization roles, invitations, OIDC SSO
          (Okta, Entra ID, Google, Keycloak) and a full auth audit trail.</p></div>
      </div>
    </section>

    <section id="deploy">
      <h2>Run it your way</h2>
      <div class="grid" style="margin-top: 24px">
        <div class="card">
          <span class="tag">Self-hosted</span>
          <h3>Free forever</h3>
          <p>MIT licensed, no feature gates, no phone-home. One
          <code class="inline">docker compose up</code> gives you the API,
          SMTP server, worker and dashboard.
          <br><br><a href="docs/self-hosting.html">Self-hosting guide →</a></p>
        </div>
        <div class="card">
          <span class="tag">Cloud</span>
          <h3>We run it, in the EU</h3>
          <p>Same open-source code, operated for you on EU infrastructure:
          managed deliverability, backups and upgrades. Simple per-email
          pricing.
          <br><br><a href="pricing.html">See pricing →</a></p>
        </div>
        <div class="card">
          <span class="tag">Developers</span>
          <h3>API-first, spec included</h3>
          <p>A complete <a href="openapi.yaml">OpenAPI specification</a> for
          all endpoints — import it into Postman, generate a client, or read
          the <a href="docs/api.html">API guide</a>.</p>
        </div>
      </div>
    </section>
""",
)

# ---------------------------------------------------------------- pricing

PAGES["pricing.html"] = dict(
    title="Pricing — CamelMailer",
    description="Self-host for free, forever. Or use the EU cloud with simple per-email pricing.",
    active="pricing.html",
    body="""
    <section class="hero" style="padding-bottom: 24px">
      <h1>Simple pricing for <em>simple mail</em>.</h1>
      <p>You pay for emails sent. Not contacts, not seats, not features.
      Every plan includes every feature — the only variable is volume.</p>
    </section>

    <section style="padding-top: 8px">
      <div class="plans">
        <div class="plan">
          <h3>Self-hosted</h3>
          <div class="price">€0 <small>forever</small></div>
          <ul>
            <li>Unlimited everything</li>
            <li>All features, MIT licensed</li>
            <li>Your infrastructure</li>
            <li>Community support</li>
          </ul>
          <a class="btn ghost" href="docs/self-hosting.html">Deploy now</a>
        </div>
        <div class="plan">
          <h3>Cloud Free</h3>
          <div class="price">€0 <small>/ month</small></div>
          <ul>
            <li>3,000 emails / month</li>
            <li>1 sending domain</li>
            <li>Full API & dashboard</li>
            <li>EU data residency</li>
          </ul>
          <a class="btn ghost" href="mailto:cloud@camelmailer.example?subject=Cloud%20beta">Join the beta</a>
        </div>
        <div class="plan featured">
          <h3>Cloud Starter</h3>
          <div class="price">€9 <small>/ month</small></div>
          <ul>
            <li>50,000 emails / month</li>
            <li>Unlimited domains & servers</li>
            <li>Webhooks, templates, inbound</li>
            <li>€0.50 per extra 1,000</li>
            <li>Email support</li>
          </ul>
          <a class="btn primary" href="mailto:cloud@camelmailer.example?subject=Cloud%20beta%20Starter">Join the beta</a>
        </div>
        <div class="plan">
          <h3>Cloud Scale</h3>
          <div class="price">€49 <small>/ month</small></div>
          <ul>
            <li>250,000 emails / month</li>
            <li>Dedicated sending IP</li>
            <li>€0.30 per extra 1,000</li>
            <li>99.9% uptime SLA & DPA</li>
            <li>Priority support</li>
          </ul>
          <a class="btn ghost" href="mailto:cloud@camelmailer.example?subject=Cloud%20beta%20Scale">Join the beta</a>
        </div>
      </div>
      <p style="margin-top:16px;color:var(--muted);font-size:13px">
        The cloud is in private beta while we finish our operations story —
        the software itself is production-ready and self-hostable today.
        Need more than 1M emails/month? <a href="mailto:cloud@camelmailer.example" style="color:var(--accent)">Talk to us.</a>
      </p>
    </section>

    <section>
      <h2>Fair-pricing FAQ</h2>
      <div class="grid" style="margin-top: 24px">
        <div class="card"><h3>What counts as an email?</h3>
          <p>One accepted recipient. A message to three recipients counts as
          three. Bounced sends still count (we did the work); suppressed
          recipients don't.</p></div>
        <div class="card"><h3>Are features gated by plan?</h3>
          <p>No. Every plan — including self-hosted — has the full feature
          set: templates, webhooks, inbound routing, SSO, everything.</p></div>
        <div class="card"><h3>Where does my data live?</h3>
          <p>Cloud plans run exclusively on EU infrastructure under EU
          jurisdiction. Self-hosted: wherever you put it.</p></div>
        <div class="card"><h3>What's the exit path?</h3>
          <p>The cloud runs the exact open-source code. Export your data,
          <code class="inline">docker compose up</code> on your own hardware,
          repoint DNS — done.</p></div>
        <div class="card"><h3>Do unused emails roll over?</h3>
          <p>No. Plans are cheap enough that rollover accounting would cost
          more than it saves you.</p></div>
        <div class="card"><h3>Marketing blasts?</h3>
          <p>No. We're built — and priced — for transactional mail. Bulk
          marketing violates the <a href="legal/acceptable-use.html">acceptable
          use policy</a> on cloud plans.</p></div>
      </div>
    </section>
""",
)

# ------------------------------------------------------------ open source

PAGES["open-source.html"] = dict(
    title="Open source — CamelMailer",
    description="MIT licensed. Born as a Rust rewrite of Postal. The cloud runs the same code you can self-host.",
    active="open-source.html",
    body="""
    <article>
      <h1>Open source, for real</h1>
      <p class="sub">MIT licensed. No open-core split, no enterprise edition,
      no delayed releases. The cloud runs the same code you can read.</p>

      <h2>License</h2>
      <p>CamelMailer is released under the <strong>MIT license</strong>. You can
      use it commercially, modify it, embed it, and redistribute it. The only
      requirement is keeping the license notice.</p>

      <h2>Heritage</h2>
      <p>CamelMailer began as a ground-up Rust rewrite of
      <a href="https://github.com/postalserver/postal">Postal</a> (also MIT), the
      long-standing open-source mail platform. We kept its proven protocol
      behaviour — the SMTP state machine is a line-for-line port, verified by
      the original test suite — and replaced the runtime: one Rust binary,
      one PostgreSQL database with row-level-security tenant isolation, and a
      new API surface focused purely on transactional mail.</p>

      <h2>What "the same code" means</h2>
      <ul>
        <li>Every feature on the cloud exists in the repository. There is no
        private fork with the good parts.</li>
        <li>Migrations, the dashboard, the template library, the OpenAPI spec
        — all in the open.</li>
        <li>If we ever disappear, your exit is
        <code class="inline">git clone</code> + <code class="inline">docker compose up</code>.</li>
      </ul>

      <h2>Contributing</h2>
      <ul>
        <li>The workspace is a standard Cargo monorepo; the frontend is Vite +
        React. <code class="inline">cargo test</code> runs the whole suite —
        PostgreSQL integration tests included when
        <code class="inline">CAMELMAILER_TEST_DATABASE_URL</code> is set.</li>
        <li>Every behaviour is covered by tests; contributions are expected to
        keep it that way (the project was built test-first).</li>
        <li>CI enforces <code class="inline">cargo fmt</code>,
        <code class="inline">clippy -D warnings</code> and the full test suite.</li>
      </ul>

      <h2>Security</h2>
      <p>Report vulnerabilities privately to
      <a href="mailto:security@camelmailer.example">security@camelmailer.example</a>.
      We ask for reasonable disclosure time and credit reporters in release
      notes. Please do not test against the cloud without permission — spin up
      your own instance instead; it's the same code.</p>

      <h2>Trademarks</h2>
      <p>"CamelMailer" and the camel logo identify the project and the managed
      cloud. Run your own instance under any name you like; just don't imply
      your service is operated by us.</p>
    </article>
""",
)

# -------------------------------------------------------------- templates

TEMPLATE_LIST = [
    ("welcome", "Welcome & getting started", "Account lifecycle"),
    ("email-verification", "Verify your email address", "Account lifecycle"),
    ("magic-link", "Passwordless sign-in link", "Account lifecycle"),
    ("password-reset", "Password reset link", "Security"),
    ("password-changed", "Password changed notice", "Security"),
    ("two-factor-code", "One-time security code", "Security"),
    ("new-device-login", "New device sign-in alert", "Security"),
    ("account-deletion", "Account deletion confirmation", "Account lifecycle"),
    ("team-invitation", "Team / workspace invitation", "Collaboration"),
    ("mention-notification", "You were mentioned", "Collaboration"),
    ("comment-reply", "New reply to your thread", "Collaboration"),
    ("order-confirmation", "Order confirmation", "Commerce"),
    ("payment-receipt", "Payment receipt", "Commerce"),
    ("payment-failed", "Payment failed (dunning)", "Commerce"),
    ("refund-processed", "Refund processed", "Commerce"),
    ("subscription-renewal", "Renewal reminder", "Commerce"),
    ("subscription-cancelled", "Cancellation confirmation", "Commerce"),
    ("trial-ending", "Trial ending reminder", "Commerce"),
    ("shipping-notification", "Order shipped + tracking", "Commerce"),
    ("data-export-ready", "Your data export is ready", "Product"),
]

_template_cards = "".join(
    f'<div class="card"><span class="tag">{category}</span><h3>{title}</h3>'
    f'<p><code class="inline">{name}</code></p></div>'
    for name, title, category in TEMPLATE_LIST
)

PAGES["templates.html"] = dict(
    title="Template library — CamelMailer",
    description="20 ready-made transactional email templates you can clone with one command.",
    active="templates.html",
    body=f"""
    <section class="hero" style="padding-bottom: 24px">
      <h1>{len(TEMPLATE_LIST)} transactional templates, <em>ready to clone</em>.</h1>
      <p>Battle-tested layouts for the mail every product sends — responsive
      HTML with plain-text twins, using simple
      <code class="inline">{{{{ variables }}}}</code>. Import them into any
      server with one command, then edit in the dashboard.</p>
    </section>
    <section style="padding-top: 0">
      <pre class="code"><span class="c"># clone the library into your mail server:</span>
git clone https://github.com/relationsoftware/postal
cd postal/camelmailer
./templates/import.sh https://mail.yourdomain.com $SERVER_API_KEY

<span class="c"># then send with any of them:</span>
curl -X POST https://mail.yourdomain.com/api/v2/server/messages/with_template \\
  -H <span class="s">"X-Server-API-Key: $SERVER_API_KEY"</span> -H <span class="s">"Content-Type: application/json"</span> \\
  -d '{{"from":"hi@yourdomain.com","to":["a@b.com"],
       "template":"welcome","template_model":{{"name":"Ada","product":"Acme","action_url":"https://app.acme.com"}}}}'</pre>
      <div class="grid" style="margin-top: 40px">{_template_cards}</div>
      <p style="margin-top:24px;color:var(--muted);font-size:14px">
        Each template ships as JSON (<code class="inline">name</code>,
        <code class="inline">subject</code>, <code class="inline">html_body</code>,
        <code class="inline">text_body</code>) in
        <a href="https://github.com/relationsoftware/postal" style="color:var(--accent)">templates/library/</a>.
        The import script is ~20 lines of curl — read it, then trust it.
      </p>
    </section>
""",
)

# ------------------------------------------------------------------ write

def main() -> None:
    here = os.path.dirname(os.path.abspath(__file__))
    from content_docs import DOC_PAGES  # docs + legal live in content_docs.py
    PAGES.update(DOC_PAGES)
    for path, page in PAGES.items():
        depth = path.count("/")
        html = layout(
            title=page["title"],
            description=page["description"],
            body=page["body"],
            depth=depth,
            active=page.get("active", ""),
        )
        target = os.path.join(here, path)
        os.makedirs(os.path.dirname(target), exist_ok=True)
        with open(target, "w") as handle:
            handle.write(html)
        print(f"wrote {path}")


if __name__ == "__main__":
    main()
