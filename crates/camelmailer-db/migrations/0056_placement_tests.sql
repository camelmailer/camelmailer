-- Inbox placement tests with seed mailboxes: rounds (one message per
-- sending IP to every seed) and the result of each test. Results also
-- become `placement` rows in feedback_snapshots.

CREATE TABLE placement_rounds (
    id BIGSERIAL PRIMARY KEY,
    trigger TEXT NOT NULL CHECK (trigger IN ('schedule', 'manual')),
    requested_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ
);

CREATE TABLE placement_tests (
    id BIGSERIAL PRIMARY KEY,
    round_id BIGINT NOT NULL REFERENCES placement_rounds (id) ON DELETE CASCADE,
    token TEXT NOT NULL UNIQUE,
    server_id BIGINT NOT NULL,
    message_id BIGINT,
    ip_pool_id BIGINT NOT NULL,
    ip_address_id BIGINT NOT NULL,
    ip TEXT NOT NULL,
    seed_address TEXT NOT NULL,
    seed_provider TEXT NOT NULL,
    provider TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'inbox', 'spam', 'missing', 'error')),
    folder TEXT,
    spf TEXT,
    dkim TEXT,
    dmarc TEXT,
    received_ip TEXT,
    helo TEXT,
    detail TEXT NOT NULL DEFAULT '',
    checks INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    checked_at TIMESTAMPTZ,
    check_at TIMESTAMPTZ NOT NULL,
    claimed_until TIMESTAMPTZ
);

CREATE INDEX idx_placement_tests_due ON placement_tests (check_at) WHERE status = 'pending';
