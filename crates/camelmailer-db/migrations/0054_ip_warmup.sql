-- Adaptive IP warm-up (off by default).
--
-- ip_pool_warmup / ip_address_warmup hold the warm-up mode set on a pool
-- (the default for its addresses) or on one address (which wins). Without a
-- row an address does not warm up, and the worker sends from it exactly as
-- before. 'custom' uses the stored profile {start, growth, cap}.
--
-- ip_warmup_state is the ramp of one address with one receiving provider
-- group: today's allowance (attempts per UTC day), the status, a pause after
-- a brake, the last day judged and why. version guards concurrent updates by
-- several workers. Changing the mode keeps the state, so the ramp continues
-- from where it is.
--
-- ip_warmup_usage counts the attempts each address made to each provider per
-- day against its allowance; ip_warmup_log records every decision (advance,
-- hold, slow, brake, warm, mode changes) with its reason.

CREATE TABLE ip_pool_warmup (
    ip_pool_id BIGINT PRIMARY KEY REFERENCES ip_pools(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('off', 'slow', 'normal', 'fast', 'custom')),
    profile JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE ip_address_warmup (
    ip_address_id BIGINT PRIMARY KEY REFERENCES ip_addresses(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('off', 'slow', 'normal', 'fast', 'custom')),
    profile JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE ip_warmup_state (
    ip_address_id BIGINT NOT NULL REFERENCES ip_addresses(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK (provider ~ '^[a-z0-9_]{1,40}$'),
    allowance BIGINT NOT NULL CHECK (allowance > 0),
    status TEXT NOT NULL CHECK (status IN ('ramping', 'holding', 'slowed', 'paused', 'warm')),
    paused_until TIMESTAMPTZ,
    evaluated_day DATE,
    last_reason TEXT NOT NULL DEFAULT '',
    last_brake_reason TEXT,
    last_brake_at TIMESTAMPTZ,
    version BIGINT NOT NULL DEFAULT 1,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (ip_address_id, provider)
);

CREATE TABLE ip_warmup_usage (
    day DATE NOT NULL,
    ip_address_id BIGINT NOT NULL REFERENCES ip_addresses(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK (provider ~ '^[a-z0-9_]{1,40}$'),
    used BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (day, ip_address_id, provider)
);

CREATE TABLE ip_warmup_log (
    id BIGSERIAL PRIMARY KEY,
    ip_address_id BIGINT NOT NULL REFERENCES ip_addresses(id) ON DELETE CASCADE,
    provider TEXT CHECK (provider ~ '^[a-z0-9_]{1,40}$'),
    decision TEXT NOT NULL,
    reason TEXT NOT NULL,
    allowance_before BIGINT,
    allowance_after BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX ip_warmup_log_address ON ip_warmup_log (ip_address_id, id DESC);
CREATE INDEX ip_warmup_log_created_at ON ip_warmup_log (created_at);
