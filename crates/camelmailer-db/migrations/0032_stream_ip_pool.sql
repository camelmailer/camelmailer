-- Phase 2 of the broadcast-stream build-out: reputation isolation via a
-- per-stream IP pool.
--
-- A message stream may now carry its own ip_pool_id. At delivery the worker
-- resolves the stream's pool first (if set), falling back to the server's
-- pool (today's behaviour) when the stream has none. This lets broadcast /
-- marketing mail send from separate IPs, protecting transactional
-- reputation. ON DELETE SET NULL mirrors servers.ip_pool_id (migration 0009):
-- removing a pool detaches the stream, it is not deleted.

ALTER TABLE message_streams
    ADD COLUMN ip_pool_id BIGINT REFERENCES ip_pools(id) ON DELETE SET NULL;
