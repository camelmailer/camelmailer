# CamelMailer web

Two frontends live here:

- **`app/`** — the admin application: React + TypeScript (Vite),
  [shadcn/ui](https://ui.shadcn.com) on Tailwind CSS v4, TanStack Query and
  React Router. Covers the full API surface: login (password + TOTP + SSO),
  organizations, RBAC member management, invitations, servers with all
  resources (domains, credentials, routes, webhooks, suppressions), and
  messaging via the per-server API (send, messages, inbound queue, stats,
  streams, templates), plus the instance-admin area (users, IP pools,
  admin API keys, audit log) and account security (password, 2FA with QR
  enrollment).
- **`marketing/`** — a dependency-free static landing page
  (`index.html`); host it anywhere.

## Development

```bash
cd web/app
pnpm install
pnpm run dev        # http://localhost:5173, proxies /api -> localhost:5000
```

The Vite dev server proxies the API, so no CORS setup is needed during
development — just have the backend running (`docker compose up -d` in the
repo root).

## Production build

```bash
pnpm run build      # -> dist/
```

Serve `dist/` from any static host. Two backend settings make it work:

```yaml
auth:
  frontend_url: https://app.your-domain.com   # invite/reset/SSO links
web_server:
  cors_origins:
    - https://app.your-domain.com             # unless served same-origin
```

Set `VITE_API_URL` at build time when the API lives on another origin
(default: same origin).

## End-to-end smoke test

`app/e2e/smoke.mjs` drives the real UI (Playwright) against the Docker
backend: login → create org/server/domain/credential → send a message →
messages/stats → invitation → audit log.

```bash
docker compose up -d                # backend on :5000 (fresh database)
docker compose exec -e CAMELMAILER_USER_PASSWORD=e2e-test-password-1 \
  web camelmailer make-user e2e@example.com E2E Tester --admin
pnpm run dev &                      # frontend on :5173
node e2e/smoke.mjs
```
