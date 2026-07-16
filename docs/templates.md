# Templates

A template is a reusable email body you store once and render with fresh
data on every send. Instead of assembling `subject`, `html_body` and
`text_body` in your application for each message, you save them as a named
template with `{{ variable }}` placeholders and pass a small JSON model at
send time. CamelMailer renders the model into the template and delivers the
result. See [Sending email](sending.md) for the send call itself.

Templates live on one mail server and are reached through the Server API
(`X-Server-API-Key`, base path `/api/v2/server`) or the dashboard under
**Server → Messaging → Templates**.

## Anatomy of a template

| Field | Type | Purpose |
|---|---|---|
| `name` | string (required) | Human label shown in the dashboard. The permalink is derived from it. |
| `permalink` | string | URL-safe slug (lowercased, hyphenated form of the name). This is the value you pass as `template` when sending, and the key in every `/templates/{permalink}` route. |
| `subject` | string / null | The subject line. May contain variables (`Order {{ order_number }} confirmed`). |
| `html_body` | string / null | The HTML part, with variables and sections. |
| `text_body` | string / null | The plain-text part, with the same variables. |
| `archived` | boolean | Archived templates stay retrievable by permalink and drop out of the active pickers. |
| `layout_id` | number / null | Optional [layout](#layouts) that wraps the rendered body. |

At least one of `subject`, `html_body` or `text_body` carries the content
you care about; `name` is the only required field on create. The permalink
is stable once created, so scripts and application code can rely on it.

## The template syntax

The renderer is a deliberately small **Mustache subset** implemented in
[`crates/camelmailer-core/src/template.rs`](../crates/camelmailer-core/src/template.rs).
It supports exactly the tags below and treats the model as untrusted
end-user data, so HTML escaping is on by default.

| Tag | Name | Behaviour |
|---|---|---|
| `{{ name }}` | Interpolation | Inserts the value, HTML-escaped. |
| `{{{ name }}}` | Raw interpolation | Inserts the value with no escaping (triple mustache). |
| `{{& name }}` | Raw interpolation | Same as triple mustache, the ampersand form. |
| `{{# section }}…{{/ section }}` | Section | Renders the body when the value is truthy; iterates when the value is an array. |
| `{{^ section }}…{{/ section }}` | Inverted section | Renders the body only when the value is falsy or missing. |
| `{{! comment }}` | Comment | Produces no output. |
| `{{ a.b.c }}` | Dotted path | Walks into nested objects. |
| `{{ . }}` | Current item | The item itself, used inside a section iterating an array of scalars. |

Whitespace inside a tag is trimmed, so `{{ name }}` and `{{name}}` are
equivalent. A section's closing name must match its opening name exactly.

### What is deliberately absent

The renderer implements the subset above and nothing more. These standard
Mustache features are intentionally left out:

- **Partials** (`{{> shared }}`). For shared chrome such as a header or
  footer, use a [layout](#layouts) instead.
- **Lambdas** and **set-delimiter** (`{{= =}}`).
- Any form of file or network access from a template.

Section nesting is capped at 32 levels and total rendered output at 512 KiB.
A template that exceeds either limit fails to render rather than running
away, which bounds the work an attacker-supplied template plus model can
cause.

### Escaping rules

`{{ name }}` escapes the five characters `& < > " '` so that a value like
`<script>` arrives as inert text. Use `{{ name }}` for anything that lands
inside HTML. Reach for `{{{ name }}}` or `{{& name }}` only when the value
is trusted HTML you want to render as markup. The plain-text body has no
markup to protect, so escaping there is harmless and you can use `{{ name }}`
throughout.

```
Template : Hi {{ name }}
Model    : { "name": "<b>Ada</b>" }
Output   : Hi &lt;b&gt;Ada&lt;/b&gt;

Template : {{{ html }}}
Model    : { "html": "<b>bold</b>" }
Output   : <b>bold</b>
```

### How values resolve

- **Missing variables render as empty.** `a{{ nope }}b` with an empty model
  produces `ab`. A typo drops a placeholder rather than raising an error.
- **Scalars render directly.** Strings render as-is, numbers and booleans
  render their text form (`42`, `true`).
- **Objects and arrays render as empty when used as a plain variable.**
  `{{ items }}` where `items` is an array produces nothing; you reach the
  contents through a section.
- **Truthiness** decides whether a section body renders: `null`, `false`,
  the empty string, and the empty array are falsy; numbers, objects,
  non-empty strings and non-empty arrays are truthy.
- **Lookup climbs the context stack.** Inside a section the current item is
  searched first, then the enclosing scopes outward, so a template can mix
  per-item fields with top-level ones such as `product`.

### Sections in practice

A section over an array repeats its body once per item, with each item as
the current scope:

```
Template : {{# items }}[{{ name }} ×{{ quantity }}]{{/ items }}
Model    : { "items": [ { "name": "Cap", "quantity": 2 },
                        { "name": "Mug", "quantity": 1 } ] }
Output   : [Cap ×2][Mug ×1]
```

An array of bare scalars uses `{{ . }}` for the item itself:

```
Template : {{# tags }}{{ . }} {{/ tags }}
Model    : { "tags": ["new", "beta"] }
Output   : new beta
```

A single flag toggles a block on or off, and its inverse fills the empty
case:

```
Template : {{# premium }}Thanks for going Pro.{{/ premium }}{{^ premium }}Upgrade any time.{{/ premium }}
Model    : { "premium": false }
Output   : Upgrade any time.
```

## HTML and plain text

A template can carry an HTML body, a plain-text body, or both. Shipping both
is the recommended shape: mail clients that render HTML get the designed
version, and clients or filters that prefer text get a clean fallback. Every
template in the bundled [library](#the-template-library) ships an HTML body
with a matching plain-text twin, and both use the same variable names so one
model fills both.

At send time you supply one model and it renders the subject, the HTML body
and the text body together. Fields you set directly on the send call (for
example `subject`) override the rendered value.

### Previews and thumbnails

The dashboard renders templates the way a client would:

- The **Templates gallery** shows each template as a live thumbnail. The
  card renders the HTML body against a sample model in a sandboxed iframe,
  so you see a real rendered mail rather than raw `{{ }}` markup. A template
  with only a text body shows a "Plain-text template" placeholder tile.
- The **sample model** behind previews is generated from the variable names
  the body references. Names ending in common patterns get plausible values:
  a name with `url` or `link` becomes an example link, `email` becomes an
  address, `name` becomes `Ada Lovelace`, `product` becomes `Acme`, `code`
  becomes `123456`, an amount or total becomes `$42.00`, and a date or
  `expires` field becomes `in 2 hours`. Any other name samples to itself.
- The **render endpoint** (`POST /api/v2/server/templates/{permalink}/render`)
  does the same server-side: pass a `template_model` and it returns the
  rendered `subject`, `html_body` and `text_body` so you can preview with
  real data before sending.

## Layouts

A layout is a reusable wrapper for the chrome every mail shares: the logo,
the postal address, the social and unsubscribe links. A template picks a
layout and the layout wraps the template's rendered body. Layouts live
alongside templates on the server (`/api/v2/server/layouts`) and in the
dashboard behind the **Layouts** button on the Templates page.

A layout has an `html_wrapper` and an optional `text_wrapper`. The wrapper
embeds the body through a raw `content` variable so the body's own HTML
survives:

```html
<table role="presentation" width="100%">
  <tr><td>{{ product }}</td></tr>
  <tr><td>{{{ content }}}</td></tr>
  <tr><td>Acme GmbH · <a href="{{ unsubscribe_url }}">Unsubscribe</a></td></tr>
</table>
```

The `{{{ content }}}` placeholder (or the `{{& content }}` form) is
required: an escaped `{{ content }}` would show the mail's markup as text,
so the editor blocks saving until a raw placeholder is present. The wrapper
sees the same model as the template plus the injected `content`, so it can
use variables such as `product` and `unsubscribe_url` too.

## The template library

CamelMailer bundles **20 production-ready transactional templates** in the
repository's [`templates/library/`](../templates/library/) directory, one
JSON file each. They cover the mail that most products send, grouped into
four areas:

| Area | Templates |
|---|---|
| Account lifecycle | `welcome`, `email-verification`, `magic-link`, `account-deletion` |
| Security | `password-reset`, `password-changed`, `two-factor-code`, `new-device-login` |
| Collaboration | `team-invitation`, `mention-notification`, `comment-reply`, `data-export-ready` |
| Commerce | `order-confirmation`, `payment-receipt`, `payment-failed`, `refund-processed`, `subscription-renewal`, `subscription-cancelled`, `trial-ending`, `shipping-notification` |

Each HTML body is a table-based, image-free, 560px layout with inline
styles, which renders consistently across mainstream clients, and every
template ships a plain-text twin. Every template expects `product` (your
product name) and `support_email`. Action mails add `action_url`, and
expiring links add `expires_in` (a human string such as `"2 hours"`). Each
JSON file lists its full variable set in a `variables` array, alongside a
`description`; both are documentation and are ignored on import.

### Import with the script

[`templates/import.sh`](../templates/import.sh) posts the library into a
server through the Server API. Give it your base URL and a Server API key:

```bash
# every template in the library
./templates/import.sh https://mail.yourdomain.com "$SERVER_KEY"

# or a chosen few, by permalink
./templates/import.sh https://mail.yourdomain.com "$SERVER_KEY" welcome password-reset
```

The script sends only the fields the API accepts (`name`, `subject`,
`html_body`, `text_body`). A template whose permalink already exists is
skipped and reported, so re-running the script is safe. To pull down a fresh
copy of one you have edited, archive or rename the existing template first.

### Import from the dashboard

The **Start from library** button on the Templates page opens the same 20
templates as a gallery wizard. Each card shows a live thumbnail, its name
and a one-line description. **Import** creates the template on the current
server; a template you already have shows as **Already imported** so you do
not duplicate it. After importing, edit it inline like any other template.

## Managing templates

### With the API

All template routes live under the Server API (`X-Server-API-Key`):

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/v2/server/templates` | List templates |
| `POST` | `/api/v2/server/templates` | Create a template (`name` required) |
| `GET` | `/api/v2/server/templates/{permalink}` | Show one template |
| `PATCH` | `/api/v2/server/templates/{permalink}` | Update fields |
| `POST` | `/api/v2/server/templates/{permalink}/archive` | Retire a template |
| `POST` | `/api/v2/server/templates/{permalink}/render` | Render against a model (preview) |

Create a template:

```bash
curl -X POST "$API/api/v2/server/templates" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{
    "name": "Welcome",
    "subject": "Welcome to {{ product }}, {{ name }}!",
    "html_body": "<h1>Hi {{ name }} 👋</h1><p><a href=\"{{ action_url }}\">Get started</a></p>",
    "text_body": "Hi {{ name }}!\n\nGet started: {{ action_url }}"
  }'
```

Update only the fields you send:

```bash
curl -X PATCH "$API/api/v2/server/templates/welcome" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"subject": "Welcome aboard, {{ name }}!"}'
```

Preview the render before you send it:

```bash
curl -X POST "$API/api/v2/server/templates/welcome/render" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{"template_model": {"product": "Acme", "name": "Ada", "action_url": "https://app.acme.com/start"}}'
# → data.rendered = { subject, html_body, text_body }
```

Retire a template by archiving it. Archiving keeps the template retrievable
by permalink and removes it from the active pickers:

```bash
curl -X POST "$API/api/v2/server/templates/welcome/archive" \
  -H "X-Server-API-Key: $SERVER_KEY"
```

To lift a template from one server into a sibling server of the same
organization, use the management API (member role or above). It copies the
whole template; a permalink clash on the target is a `422` unless you pass
`overwrite: true`:

```bash
curl -X POST "$API/api/v2/admin/organizations/acme/servers/production/templates/welcome/copy_to" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"target_server": "staging", "overwrite": false}'
```

### In the dashboard

Under **Server → Messaging → Templates** you get the gallery of thumbnails,
each with its name, permalink and a Published or Archived pill. From here:

- **New template** and **Edit** open a focus-mode split editor where you
  write the subject, HTML and text bodies and preview the live render.
- **Copy to server…** pushes the template to another server of the same
  organization.
- **Archive** retires a published template.
- **Layouts** and **Start from library** open the wrappers manager and the
  bundled-template wizard.

## Sending with a template

Rendering and sending happen in one call to
`POST /api/v2/server/messages/with_template`. Name the template by permalink
and pass the `template_model`:

```bash
curl -X POST "$API/api/v2/server/messages/with_template" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{
    "from": "hello@acme.example",
    "to": ["ada@example.com"],
    "template": "welcome",
    "template_model": {
      "product": "Acme",
      "support_email": "support@acme.com",
      "name": "Ada",
      "action_url": "https://app.acme.com/start"
    }
  }'
```

The `order-confirmation` template shows a model with a list. Its body
iterates the `items` array with a section while pulling `order_number` and
`total` from the top level:

```json
{
  "from": "orders@acme.example",
  "to": ["ada@example.com"],
  "template": "order-confirmation",
  "template_model": {
    "product": "Acme",
    "support_email": "support@acme.com",
    "name": "Ada",
    "order_number": "A-1042",
    "items": [
      { "name": "Camel cap", "quantity": 2, "price": "$18.00" },
      { "name": "Enamel mug", "quantity": 1, "price": "$12.00" }
    ],
    "total": "$48.00",
    "action_url": "https://app.acme.com/orders/A-1042"
  }
}
```

Fields set directly on the send call override the rendered ones, so passing
`subject` alongside `template` replaces the template's subject for that one
message. To reach many recipients with per-recipient models, use the batch
form, `POST /api/v2/server/messages/with_template/batch`.

The dashboard offers the same flow: the **Send a message** dialog has a
Template picker that, once you choose a template, shows one field per
referenced variable (pre-filled with sample hints) and an expert mode for
editing the whole model as JSON.

## Related

- [Sending email](sending.md) covers the send calls, senders and delivery.
- [Campaigns](campaigns.md) and [Broadcast streams](broadcast.md) reuse
  templates for bulk and stream-based sending.
</content>
</invoke>
