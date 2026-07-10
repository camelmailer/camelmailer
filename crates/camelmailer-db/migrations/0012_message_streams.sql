-- Message streams: Postmark-style flat labels that group outbound/inbound
-- mail (transactional vs broadcast vs inbound). A config table like servers
-- and routes — NOT RLS-protected — so every query filters on server_id
-- explicitly. Streams are flat labels: no reply-threading, no hierarchy.

CREATE TABLE message_streams (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    permalink TEXT NOT NULL,
    stream_type TEXT NOT NULL DEFAULT 'transactional'
        CHECK (stream_type IN ('transactional', 'broadcast', 'inbound')),
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, permalink)
);
CREATE INDEX idx_message_streams_server ON message_streams (server_id);

ALTER TABLE messages ADD COLUMN stream_id BIGINT;
CREATE INDEX idx_messages_stream ON messages (server_id, stream_id);

-- Backfill a default transactional stream per existing server and point
-- servers.default_stream_id at it (matches Postmark's built-in "outbound").
INSERT INTO message_streams (uuid, server_id, name, permalink, stream_type)
SELECT gen_random_uuid()::text, id, 'Default Transactional Stream', 'outbound', 'transactional'
FROM servers;

UPDATE servers s
SET default_stream_id = ms.id
FROM message_streams ms
WHERE ms.server_id = s.id AND ms.permalink = 'outbound';
