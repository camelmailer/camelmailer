-- Users, IP pools/addresses, webhooks (global/config data) and
-- suppressions (tenant-scoped, RLS-protected like messages).

CREATE TABLE users (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    email_address TEXT NOT NULL UNIQUE,
    first_name TEXT NOT NULL,
    last_name TEXT NOT NULL,
    admin BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE ip_pools (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    "default" BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE ip_addresses (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    ip_pool_id BIGINT NOT NULL REFERENCES ip_pools(id) ON DELETE CASCADE,
    ipv4 TEXT NOT NULL,
    ipv6 TEXT,
    hostname TEXT NOT NULL,
    priority INTEGER NOT NULL DEFAULT 100,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE webhooks (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    url TEXT NOT NULL,
    all_events BOOLEAN NOT NULL DEFAULT TRUE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    sign BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_webhooks_server ON webhooks (server_id);

-- Suppressions lived in the per-server message database in the Ruby app;
-- here they share the tenant-isolation model of messages.
CREATE TABLE suppressions (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    type TEXT NOT NULL DEFAULT 'recipient',
    address TEXT NOT NULL,
    reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, address)
);

ALTER TABLE suppressions ENABLE ROW LEVEL SECURITY;
ALTER TABLE suppressions FORCE ROW LEVEL SECURITY;

CREATE POLICY suppressions_tenant_isolation ON suppressions
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);
