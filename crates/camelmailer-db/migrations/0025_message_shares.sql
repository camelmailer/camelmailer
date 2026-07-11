-- Public share links for message details.
--
-- A cross-tenant lookup table like tracking_tokens: the public share
-- endpoint is unauthenticated and receives only a token, so it needs a
-- non-RLS index to resolve which tenant to enter. Only the SHA-256 hash
-- of the share token is stored — a database leak does not leak live
-- share links. The message data itself is still read through the
-- RLS-protected tables under the resolved tenant context.

CREATE TABLE message_shares (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    message_id BIGINT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_message_shares_server ON message_shares (server_id, message_id);
