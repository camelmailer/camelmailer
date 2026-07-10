-- Webhook delivery: a cross-tenant work queue (same design as
-- queued_messages — the worker's work list, no RLS) plus a tenant-scoped,
-- RLS-protected audit log of every attempt (the port of Postal's
-- webhook_requests history in the per-server message database).

CREATE TABLE webhook_requests (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    webhook_id BIGINT NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    uuid TEXT NOT NULL,
    event TEXT NOT NULL,
    url TEXT NOT NULL,
    payload TEXT NOT NULL,
    sign BOOLEAN NOT NULL DEFAULT TRUE,
    attempts INTEGER NOT NULL DEFAULT 0,
    retry_after TIMESTAMPTZ,
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_webhook_requests_ready ON webhook_requests (retry_after, id)
    WHERE locked_by IS NULL;

CREATE TABLE webhook_request_log (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    webhook_id BIGINT,
    uuid TEXT NOT NULL,
    event TEXT NOT NULL,
    url TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    status_code INTEGER,
    success BOOLEAN NOT NULL,
    response_body TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_webhook_request_log_server ON webhook_request_log (server_id, id);

ALTER TABLE webhook_request_log ENABLE ROW LEVEL SECURITY;
ALTER TABLE webhook_request_log FORCE ROW LEVEL SECURITY;
CREATE POLICY webhook_request_log_tenant_isolation ON webhook_request_log
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);
