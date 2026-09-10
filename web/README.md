# Camelmailer web

One **Next.js** application (App Router) serves both faces of the product:

- **Marketing** — `/`, `/pricing`, `/templates`, `/open-source`,
  `/docs/api`, `/docs/self-hosting`, `/legal/*` and the public
  `/openapi.yaml`: statically prerendered, scoped styles, no client JS
  beyond Next itself.
- **App** — `/login` (+ password reset, invitation accept, SSO callback)
  and the signed-in area under `/dashboard`, `/orgs/[org]`,
  `/orgs/[org]/servers/[server]` (settings, domains, credentials, routes,
  webhooks, suppressions, messaging), `/account`, `/admin/*`: client
  components built on shadcn/ui (Tailwind v4), TanStack Query and the
  typed API client in `src/lib/api.ts`.

## Development

```bash
cd web/app
pnpm install
pnpm run dev        # http://localhost:3000
```

The Next server proxies `/api` and `/health` to the backend
(`API_PROXY_URL`, default `http://localhost:5000`) — no CORS setup needed.
Have the backend running: `docker compose up -d` in the repo root.

## Production

```bash
pnpm run build
API_PROXY_URL=https://mail.internal:5000 pnpm run start
```

Because the Next server proxies the API, the app is same-origin in
production too — `web_server.cors_origins` stays empty. Set
`auth.frontend_url` on the backend to this app's public URL so
invitation/reset links and the SSO handoff point here. (Alternative: skip
the proxy, set `NEXT_PUBLIC_API_URL` at build time and configure CORS.)

## Layout

```
src/app/            routes (App Router)
  (app)/            signed-in area (layout = session gate + sidebar shell)
  login/, reset-password/, invitations/accept/, auth/callback/
src/views/          the page components (client), shared by the routes
src/components/     shadcn/ui + shared building blocks
src/lib/            api client, auth context, params helper
public/openapi.yaml the public OpenAPI spec
```

## End-to-end smoke test

`app/e2e/smoke.mjs` (Playwright) drives the real UI against the Docker
backend: login → org/server/domain/credential →
send → message detail → stats → invitation → audit log.

```bash
docker compose up -d
docker compose exec -e CAMELMAILER_USER_PASSWORD=e2e-test-password-1 \
  web camelmailer make-user e2e@example.com E2E Tester --admin
pnpm run dev &
node e2e/smoke.mjs
```

## Focused browser tests

From `web/app`, run `pnpm exec playwright install chromium` once, then
`pnpm test:ui`. The Playwright test runner starts the dashboard at
`http://127.0.0.1:3217` and mocks API responses in the browser, so these tests
need no backend or test account. Set `E2E_BASE_URL` to use an already-running
app instead. Failed tests save screenshots and traces under `test-results/`.

The bounce tests cover the original-message link, keyboard navigation,
unmatched notifications, and an original message that is no longer available.
These check UI rendering and routing; the Rust integration tests cover actual
bounce correlation and tenant isolation. The separate `e2e/smoke.mjs` script
above exercises the real backend.
