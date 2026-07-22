# The CamelMailer web dashboard (Next.js), published as
# ghcr.io/camelmailer/camelmailer-web alongside the engine image.
#
# The Next server proxies /api and /health to the backend, and Next bakes
# rewrites() into the build output (routes-manifest) — the proxy target is
# therefore fixed at BUILD time, not at runtime. The default targets a
# compose service named `web` on port 5000 (the shape of
# install/docker-compose.yml); build with
#   --build-arg API_PROXY_URL=http://your-backend:5000
# to point elsewhere.
#
# Build context: this directory (web/app).

FROM node:22-alpine AS build
WORKDIR /app
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile
COPY . .
ARG API_PROXY_URL=http://web:5000
ENV API_PROXY_URL=$API_PROXY_URL
RUN pnpm run build

FROM node:22-alpine
WORKDIR /app
ENV NODE_ENV=production
RUN corepack enable
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile --prod
COPY --from=build /app/.next ./.next
COPY --from=build /app/public ./public
COPY next.config.ts ./
EXPOSE 3000
CMD ["pnpm", "start"]
