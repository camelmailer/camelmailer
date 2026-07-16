-- Phase 4 of the broadcast-stream build-out: opt-in / consent enforcement.
--
-- A subscription records a recipient's consent for one (server, stream,
-- address). Broadcast (marketing) streams may only send to an address with a
-- `subscribed` row; unsubscribing flips the status to `unsubscribed` (in
-- addition to the stream-scoped suppression record_unsubscribe already writes).
-- This is tenant data, so it shares the messages/suppressions RLS model.

CREATE TABLE subscriptions (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    stream_id BIGINT NOT NULL REFERENCES message_streams(id) ON DELETE CASCADE,
    address TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'subscribed'
        CHECK (status IN ('subscribed', 'unsubscribed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, stream_id, address)
);

ALTER TABLE subscriptions ENABLE ROW LEVEL SECURITY;
ALTER TABLE subscriptions FORCE ROW LEVEL SECURITY;

CREATE POLICY subscriptions_tenant_isolation ON subscriptions
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);
