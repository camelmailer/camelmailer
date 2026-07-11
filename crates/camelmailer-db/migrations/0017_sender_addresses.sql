-- Per-address sender signatures: a single email address a server may
-- send From once the address holder confirms the emailed token. A config
-- table like webhooks — NOT RLS-protected — every query filters on
-- server_id explicitly (or on the token hash for the public confirm).

CREATE TABLE sender_addresses (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    email_address TEXT NOT NULL,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    verification_token_hash TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, email_address)
);
CREATE INDEX idx_sender_addresses_server ON sender_addresses (server_id);
CREATE INDEX idx_sender_addresses_token ON sender_addresses (verification_token_hash);
