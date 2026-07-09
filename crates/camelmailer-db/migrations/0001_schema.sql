-- CamelMailer schema — a single PostgreSQL database for all tenants.
--
-- This replaces the MariaDB layout of the Ruby application, which used a
-- `main_db` for configuration plus one dedicated MariaDB database *per mail
-- server* for message storage (`lib/postal/message_db/`). Messages now live
-- in one shared table, isolated per server with row-level security (see the
-- following migration).

CREATE TABLE organizations (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    permalink TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE servers (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    organization_id BIGINT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    permalink TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    mode TEXT NOT NULL DEFAULT 'Live' CHECK (mode IN ('Live', 'Development')),
    suspended BOOLEAN NOT NULL DEFAULT FALSE,
    suspension_reason TEXT,
    privacy_mode BOOLEAN NOT NULL DEFAULT FALSE,
    log_smtp_data BOOLEAN NOT NULL DEFAULT FALSE,
    allow_sender BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, permalink)
);

CREATE TABLE domains (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    owner_type TEXT NOT NULL CHECK (owner_type IN ('Organization', 'Server')),
    owner_id BIGINT NOT NULL,
    name TEXT NOT NULL,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (owner_type, owner_id, name)
);
CREATE INDEX idx_domains_name ON domains (name);

CREATE TABLE routes (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    domain_id BIGINT REFERENCES domains(id) ON DELETE SET NULL,
    name TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    mode TEXT NOT NULL DEFAULT 'Endpoint'
        CHECK (mode IN ('Endpoint', 'Accept', 'Hold', 'Bounce', 'Reject')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_routes_server ON routes (server_id);

CREATE TABLE credentials (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    type TEXT NOT NULL CHECK (type IN ('SMTP', 'API', 'SMTP-IP')),
    name TEXT NOT NULL,
    key TEXT NOT NULL,
    hold BOOLEAN NOT NULL DEFAULT FALSE,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_credentials_type_key ON credentials (type, key);

CREATE TABLE admin_api_keys (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    key TEXT NOT NULL UNIQUE,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE messages (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    scope TEXT NOT NULL CHECK (scope IN ('incoming', 'outgoing')),
    rcpt_to TEXT NOT NULL,
    mail_from TEXT NOT NULL,
    bounce BOOLEAN NOT NULL DEFAULT FALSE,
    received_with_ssl BOOLEAN NOT NULL DEFAULT FALSE,
    domain_id BIGINT,
    credential_id BIGINT,
    route_id BIGINT,
    raw_message BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_messages_server ON messages (server_id, id);
