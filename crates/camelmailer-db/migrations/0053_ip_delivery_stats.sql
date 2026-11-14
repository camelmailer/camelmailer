-- Delivery counters per sending address and receiving provider.
--
-- ip_delivery_stats counts every outbound SMTP result per UTC day, per
-- source address (the IPv4 value of the IP-pool address the delivery was
-- bound to, or 'default' for servers without a pool) and per receiving
-- provider group (microsoft, google, yahoo, other). It feeds the IP metrics
-- of the infrastructure dashboard and the adaptive warm-up. The worker
-- writes it after the result of a delivery is committed; a failed write is
-- logged and never affects the delivery.
--
-- message_sources remembers, for a few days, which address sent each
-- accepted message, so a spam complaint (feedback loop) can be counted
-- against that address. Both tables are pruned by the worker's
-- housekeeping.

CREATE TABLE ip_delivery_stats (
    day DATE NOT NULL,
    source_ip TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('microsoft', 'google', 'yahoo', 'other')),
    ip_address_id BIGINT,
    sent BIGINT NOT NULL DEFAULT 0,
    deferred BIGINT NOT NULL DEFAULT 0,
    bounced BIGINT NOT NULL DEFAULT 0,
    policy_blocks BIGINT NOT NULL DEFAULT 0,
    complaints BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (day, source_ip, provider)
);

CREATE TABLE message_sources (
    message_id BIGINT PRIMARY KEY,
    server_id BIGINT NOT NULL,
    source_ip TEXT NOT NULL,
    ip_address_id BIGINT,
    provider TEXT NOT NULL CHECK (provider IN ('microsoft', 'google', 'yahoo', 'other')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX message_sources_created_at ON message_sources (created_at);
