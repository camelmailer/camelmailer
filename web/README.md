# CamelMailer web

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
  (marketing)/      static pages + scoped marketing.css + content.ts
  (app)/            signed-in area (layout = session gate + sidebar shell)
  login/, reset-password/, invitations/accept/, auth/callback/
src/views/          the page components (client), shared by the routes
src/components/     shadcn/ui + shared building blocks
src/lib/            api client, auth context, params helper
public/openapi.yaml the public OpenAPI spec
```

## End-to-end smoke test

`app/e2e/smoke.mjs` (Playwright) drives the real UI against the Docker
backend: marketing landing → login → org/server/domain/credential →
send → message detail → stats → invitation → audit log.

```bash
docker compose up -d
docker compose exec -e CAMELMAILER_USER_PASSWORD=e2e-test-password-1 \
  web camelmailer make-user e2e@example.com E2E Tester --admin
pnpm run dev &
node e2e/smoke.mjs
```
