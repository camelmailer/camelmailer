# Transactional template library

Twenty ready-made templates for the mail every product sends — account
lifecycle, security, collaboration, commerce. Each is a single JSON file
in [`library/`](library/) with:

| Field | Notes |
|---|---|
| `name` | Becomes the template name + permalink (what you pass as `template` when sending) |
| `subject`, `html_body`, `text_body` | The template content, using the Mustache-subset (`{{ variable }}`, `{{#section}}…{{/section}}`) |
| `description`, `variables` | Documentation only — ignored by the import |

The HTML is a consistent, image-free, table-based 560px layout with inline
styles (renders in every mainstream client), and every template ships a
plain-text twin.

## Conventions

Every template expects `product` (your product name, used as the header
"logo" and in copy) and `support_email`. Action mails add `action_url`;
expiring links add `expires_in` (a human string like `"2 hours"`). The
full variable list is in each file's `variables` array.

## Import into a server

```bash
./templates/import.sh https://mail.yourdomain.com $SERVER_API_KEY          # all 20
./templates/import.sh https://mail.yourdomain.com $SERVER_API_KEY welcome password-reset
```

Existing templates (same permalink) are skipped, never overwritten. After
importing, edit in the dashboard (Server → Messaging → Templates) or via
`PATCH /api/v2/server/templates/{permalink}`, and preview with
`POST /api/v2/server/templates/{permalink}/render`.

## Send with one

```bash
curl -X POST https://mail.yourdomain.com/api/v2/server/messages/with_template \
  -H "X-Server-API-Key: $SERVER_API_KEY" -H "Content-Type: application/json" \
  -d '{
    "from": "hello@yourdomain.com",
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

## The templates

| Permalink | Purpose |
|---|---|
| `welcome` | Post-signup greeting + first action |
| `email-verification` | Confirm address ownership |
| `magic-link` | Passwordless sign-in |
| `password-reset` | Reset link |
| `password-changed` | Security notice after change |
| `two-factor-code` | One-time security code |
| `new-device-login` | New device/location alert |
| `account-deletion` | Deletion scheduled + grace period |
| `team-invitation` | Workspace invite |
| `mention-notification` | You were mentioned |
| `comment-reply` | New reply in your thread |
| `order-confirmation` | Order received (items via `{{#items}}`) |
| `payment-receipt` | Payment succeeded |
| `payment-failed` | Dunning + retry date |
| `refund-processed` | Refund issued |
| `subscription-renewal` | Upcoming renewal |
| `subscription-cancelled` | Cancellation confirmed |
| `trial-ending` | Trial expiry reminder |
| `shipping-notification` | Shipped + tracking |
| `data-export-ready` | Export download (expiring) |
