-- The delivery queue (port of the main-DB `queued_messages` table).
--
-- Deliberately NOT RLS-protected: the queue is the worker's cross-tenant
-- work list, mirroring Postal's design where QueuedMessage lives in the
-- main database while message bodies live in the per-server databases.
-- The worker never reads message *content* through the queue — it enters
-- the owning server's RLS tenant context to load each message, so tenant
-- isolation on message data stays intact and no BYPASSRLS role is needed.

CREATE TABLE queued_messages (
    id BIGSERIAL PRIMARY KEY,
    message_id BIGINT NOT NULL,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    -- destination domain (batch/routing key)
    domain TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    retry_after TIMESTAMPTZ,
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_queued_messages_ready ON queued_messages (retry_after, id)
    WHERE locked_by IS NULL;

-- Incoming-route delivery target. A deliberate simplification of Postal's
-- polymorphic endpoints (http/smtp/address): a route can carry an HTTP
-- endpoint URL directly.
ALTER TABLE routes ADD COLUMN endpoint_url TEXT;
