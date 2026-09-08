-- Completed send submissions retained for 24-hour idempotent replay.
-- Only SHA-256 hashes of the caller's key and normalized request are stored;
-- the response contains the generated message ids and public tokens that a
-- retry must receive again.

CREATE TABLE idempotency_requests (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    key_hash TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    operation TEXT NOT NULL,
    response JSONB NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT idempotency_requests_server_key_key UNIQUE (server_id, key_hash)
);

CREATE INDEX idx_idempotency_requests_expires
    ON idempotency_requests (expires_at);

ALTER TABLE idempotency_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE idempotency_requests FORCE ROW LEVEL SECURITY;
CREATE POLICY idempotency_requests_tenant_isolation ON idempotency_requests
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);
