-- Phase 1 of the broadcast-stream build-out: stream-scoped suppressions and
-- one-click unsubscribe tokens.
--
-- A suppression now carries an optional stream_id: NULL means server-wide
-- (hard bounces, manual suppressions — today's behaviour), a set stream_id
-- scopes the opt-out to a single message stream (marketing unsubscribes must
-- never block critical transactional mail). A recipient is blocked for a
-- message when a suppression exists with stream_id IS NULL OR
-- stream_id = message.stream_id.

ALTER TABLE suppressions
    ADD COLUMN stream_id BIGINT REFERENCES message_streams(id) ON DELETE CASCADE;

-- Replace the flat UNIQUE (server_id, address) with one that also keys on the
-- stream, so a recipient can be suppressed server-wide (NULL) AND per-stream.
-- COALESCE(stream_id, 0) collapses the server-wide row to a stable sentinel
-- (0 is never a valid message_streams.id).
ALTER TABLE suppressions DROP CONSTRAINT suppressions_server_id_address_key;
CREATE UNIQUE INDEX suppressions_server_addr_stream
    ON suppressions (server_id, address, COALESCE(stream_id, 0));

-- One-click unsubscribe tokens. Like tracking_tokens, this is a cross-tenant
-- lookup table resolved by the opaque token alone (the public unsubscribe
-- endpoint is unauthenticated and carries no tenant context), so it is NOT
-- RLS-protected.
CREATE TABLE unsubscribe_tokens (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    stream_id BIGINT REFERENCES message_streams(id) ON DELETE CASCADE,
    address TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
