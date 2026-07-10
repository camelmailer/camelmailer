-- A cross-tenant lookup table mapping public tracking tokens to their
-- owning (server, message[, link]). The tracking HTTP endpoints are
-- unauthenticated and receive only a token, so they need a non-RLS index
-- to resolve which tenant to enter — the same pattern as queued_messages.
-- The recorded click/load rows still land in the RLS-protected link_clicks
-- and loads tables under the resolved tenant context.

CREATE TABLE tracking_tokens (
    token TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('click', 'open')),
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    message_id BIGINT NOT NULL,
    link_id BIGINT,
    target_url TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
