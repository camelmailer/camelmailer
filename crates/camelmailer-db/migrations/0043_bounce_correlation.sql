-- Link an inbound delivery-status notification to the outgoing message
-- identified by its returned X-CamelMailer-MsgID header. The worker also
-- accepts X-Postal-MsgID on mail migrated from Postal.

ALTER TABLE messages
    ADD COLUMN bounce_for_id BIGINT,
    ADD COLUMN bounce_correlated_at TIMESTAMPTZ;

CREATE INDEX idx_messages_server_token ON messages (server_id, token);
CREATE INDEX idx_messages_bounce_for ON messages (server_id, bounce_for_id)
    WHERE bounce_for_id IS NOT NULL;
