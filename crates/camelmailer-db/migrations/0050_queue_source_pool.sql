-- The IP pool an outgoing message sends from, resolved at enqueue time (the
-- stream's pool, else the server's). A worker that only holds some pools'
-- addresses skips rows of the others instead of dequeuing and handing them
-- back. NULL (incoming mail, rows queued before this column, no pool) is
-- served by every worker, as before.
ALTER TABLE queued_messages ADD COLUMN source_pool_id BIGINT;
