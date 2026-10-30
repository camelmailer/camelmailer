-- Organization quota, suspension, monthly usage and installation lifecycle
-- events.
--
-- organizations.monthly_send_limit caps outgoing messages per calendar month
-- (UTC) across every server of the organization. NULL means unlimited, which
-- is what every existing organization gets, so this migration changes no
-- behaviour on its own. organizations.suspended stops outgoing mail from all
-- of the organization's servers.
--
-- organization_send_counters is the authoritative monthly usage figure. It is
-- incremented in the transaction that stores an outgoing message (next to
-- server_send_counters) and is never pruned: one row per organization and
-- month, read by billing. Like server_send_counters it holds accounting, not
-- message content, so it sits outside row-level security.
--
-- organization_usage_alerts remembers which usage thresholds (80 and 100
-- percent) were already announced in a month, so each fires once.
--
-- lifecycle_events is the outbox the worker delivers to
-- lifecycle_webhooks.urls. It has no foreign key on purpose: an
-- organization.deleted event must outlive its organization.

ALTER TABLE organizations ADD COLUMN monthly_send_limit BIGINT;
ALTER TABLE organizations ADD COLUMN suspended BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE organizations ADD COLUMN suspension_reason TEXT;

COMMENT ON COLUMN organizations.billing_customer_id IS
    'Customer id at the billing provider of the installation (provider neutral)';

CREATE TABLE organization_send_counters (
    organization_id BIGINT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    month DATE NOT NULL,
    sent BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (organization_id, month)
);

CREATE TABLE organization_usage_alerts (
    organization_id BIGINT NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    month DATE NOT NULL,
    threshold SMALLINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, month, threshold)
);

CREATE TABLE lifecycle_events (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    event TEXT NOT NULL,
    payload TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    attempts INTEGER NOT NULL DEFAULT 0,
    retry_after TIMESTAMPTZ,
    locked_by TEXT,
    locked_at TIMESTAMPTZ,
    last_error TEXT
);

CREATE INDEX idx_lifecycle_events_ready ON lifecycle_events (retry_after, id);
