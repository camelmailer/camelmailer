-- Fair delivery queue.
--
-- The worker used to take the oldest ready row of the whole queue, so one
-- server's 10,000 queued campaign mails delayed every other server's password
-- resets behind them. It now takes turns between servers, and within a server
-- it delivers transactional mail before bulk mail.
--
-- queued_messages.bulk marks a message sent on a broadcast stream (campaigns
-- go through broadcast streams too). It is set when the message is queued.
-- Existing rows default to false, which keeps their old first-in-first-out
-- order; a queue drains within minutes, so no backfill is needed.
--
-- The index serves the per-server pick: the next ready row of one server,
-- transactional first, oldest first.

ALTER TABLE queued_messages ADD COLUMN bulk BOOLEAN NOT NULL DEFAULT false;

CREATE INDEX idx_queued_messages_server_next
    ON queued_messages (server_id, bulk, id);
