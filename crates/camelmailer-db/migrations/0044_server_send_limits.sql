-- Per-server send limits.
--
-- servers.send_limit is the number of outgoing messages a server may store in
-- the trailing 30-day window. NULL means unlimited, which is what every
-- existing server gets, so this migration changes no behaviour on its own.
--
-- Usage is counted in daily buckets rather than by counting rows in
-- `messages`. Two reasons: a COUNT over a 30-day window runs on the send hot
-- path, and `message_retention_days` deletes old messages, which would hand a
-- server its quota back early. A bucket row outlives the messages it counted.
--
-- The table is deliberately outside row-level security, like `servers` itself:
-- it holds accounting per server, not message content.

ALTER TABLE servers ADD COLUMN send_limit BIGINT;

CREATE TABLE server_send_counters (
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    day DATE NOT NULL,
    sent BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (server_id, day)
);

CREATE INDEX idx_server_send_counters_day ON server_send_counters (day);
