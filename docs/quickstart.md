# Quickstart — zero to first mail

This walkthrough boots the full CamelMailer stack with Docker Compose and
sends a message over the HTTP API. Total time: about five minutes, most of
it the first image build.

## 1. Boot the stack

```bash
git clone https://github.com/YOUR-ORG/camelmailer && cd camelmailer
cp .env.example .env          # set POSTGRES_PASSWORD to anything
docker compose up -d --build
```

This starts PostgreSQL, runs the schema migrations, and launches the three
CamelMailer processes:

| Service | Role | Port |
|---|---|---|
| `web` | HTTP APIs (Admin + Server + tracking) | 5000 |
| `smtp` | SMTP intake (submission + inbound MX) | 25 |
| `worker` | Delivery, DKIM, tracking, webhooks | — |

Check it's alive:

```bash
curl -s http://localhost:5000/health
# {"status":"ok","version":"…"}
```

## 2. Create an admin API key

```bash
docker compose exec web camelmailer make-admin-api-key ops
```

Copy the printed key — it authenticates the **Account API**
(`X-Admin-API-Key` header, `/api/v2/admin/...`). Export it for the rest of
this guide:

```bash
export ADMIN_KEY=…   # the key printed above
export API=http://localhost:5000
```

## 3. Create an organization, server, and domain

```bash
# organization
curl -s -X POST "$API/api/v2/admin/organizations" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"name": "Acme"}'

# mail server inside it
curl -s -X POST "$API/api/v2/admin/organizations/acme/servers" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"name": "Production"}'

# sending domain
curl -s -X POST "$API/api/v2/admin/organizations/acme/servers/production/domains" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"name": "acme.example"}'

# mark it verified (real DNS verification is on the roadmap;
# in production, publish the SPF/DKIM records first — see configuration.md)
curl -s -X POST "$API/api/v2/admin/organizations/acme/servers/production/domains/acme.example/verify" \
  -H "X-Admin-API-Key: $ADMIN_KEY"
```

## 4. Create a server API token

Per-server credentials of type `API` are the tokens for the **Server API**
(`X-Server-API-Key` header, `/api/v2/server/...`):

```bash
curl -s -X POST "$API/api/v2/admin/organizations/acme/servers/production/credentials" \
  -H "X-Admin-API-Key: $ADMIN_KEY" -H "Content-Type: application/json" \
  -d '{"type": "API", "name": "backend"}'
```

The response contains the token in `data.credential.key`:

```bash
export SERVER_KEY=…   # data.credential.key from above
```

## 5. Send your first message

```bash
curl -s -X POST "$API/api/v2/server/messages" \
  -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
  -d '{
    "from": "hello@acme.example",
    "to": ["you@example.com"],
    "subject": "Hello from CamelMailer",
    "html_body": "<p>It works 🐫</p>",
    "text_body": "It works"
  }'
```

The message is stored, queued, and the worker delivers it (direct-to-MX, or
via `smtp_relays` if configured). Watch it happen:

```bash
docker compose logs -f worker
```

## 6. Read it back

```bash
# list + filter
curl -s "$API/api/v2/server/messages?query=hello" -H "X-Server-API-Key: $SERVER_KEY"

# one message with its delivery attempts
curl -s "$API/api/v2/server/messages/1" -H "X-Server-API-Key: $SERVER_KEY"

# aggregate statistics
curl -s "$API/api/v2/server/stats" -H "X-Server-API-Key: $SERVER_KEY"
```

## Where to go next

- **[configuration.md](configuration.md)** — config file, DKIM signing key,
  DNS records, TLS, relays, production checklist.
- **README** — the full API surface (streams, templates, inbound, bounces,
  webhooks, suppressions) and architecture notes.
- Templates in 30 seconds:

  ```bash
  curl -s -X POST "$API/api/v2/server/templates" \
    -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
    -d '{"name": "Welcome", "subject": "Hi {{ name }}", "text_body": "Welcome, {{ name }}!"}'

  curl -s -X POST "$API/api/v2/server/messages/with_template" \
    -H "X-Server-API-Key: $SERVER_KEY" -H "Content-Type: application/json" \
    -d '{"from": "hello@acme.example", "to": ["you@example.com"],
         "template": "welcome", "template_model": {"name": "Ada"}}'
  ```

## Running without Docker

CamelMailer is a single static-ish Rust binary; all you need is PostgreSQL:

```bash
cargo build --release -p camelmailer
export DATABASE_URL=postgres://camelmailer:secret@localhost:5432/camelmailer
./target/release/camelmailer initialize
./target/release/camelmailer web-server &
./target/release/camelmailer smtp-server &
./target/release/camelmailer worker &
```
