-- Server-level configuration parity (Postmark-style server settings, in
-- native shape): tracking defaults, spam thresholds, hook URLs, inbound
-- domain, UI color, and the default message stream (populated in a later
-- migration). Plus a message tag for categorization/search.

ALTER TABLE servers
    ADD COLUMN track_opens BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN track_clicks BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN spam_threshold DOUBLE PRECISION,
    ADD COLUMN outbound_spam_threshold DOUBLE PRECISION,
    ADD COLUMN bounce_hook_url TEXT,
    ADD COLUMN delivery_hook_url TEXT,
    ADD COLUMN inbound_domain TEXT,
    ADD COLUMN color TEXT,
    ADD COLUMN default_stream_id BIGINT;

ALTER TABLE messages ADD COLUMN tag TEXT;
CREATE INDEX idx_messages_tag ON messages (server_id, tag) WHERE tag IS NOT NULL;
