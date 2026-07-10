-- Associate a server with an IP pool so outbound mail can be sent from a
-- source address in that pool (port of Postal's ip_pool assignment).

ALTER TABLE servers ADD COLUMN ip_pool_id BIGINT REFERENCES ip_pools(id) ON DELETE SET NULL;
