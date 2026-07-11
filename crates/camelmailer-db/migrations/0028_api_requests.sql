-- Per-server API request log (the Resend "/logs" pattern): one metadata
-- row per authenticated request to /api/v2/server/*. Deliberately no
-- bodies, no API keys and no query strings — method, path, status code,
-- duration and a truncated user agent only.
--
-- Tenant scoping: like `queued_messages`, this table is deliberately NOT
-- RLS-protected — the worker's retention job deletes expired rows across
-- all tenants in one statement, which FORCE row-level security would
-- block. Every read path MUST therefore filter on `server_id` explicitly
-- (enforced in `PgStore::api_requests`; there is no other reader).

CREATE TABLE api_requests (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    method TEXT NOT NULL,
    path TEXT NOT NULL,
    status_code INTEGER NOT NULL,
    duration_ms BIGINT NOT NULL,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- serves both the per-server listing (newest first) and the retention
-- delete (created_at range scan)
CREATE INDEX idx_api_requests_server_created ON api_requests (server_id, created_at DESC);
CREATE INDEX idx_api_requests_created ON api_requests (created_at);
