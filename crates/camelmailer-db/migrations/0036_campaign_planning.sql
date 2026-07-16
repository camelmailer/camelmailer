-- Campaign planning: promote campaigns from "created = sent immediately" to a
-- planned lifecycle. A campaign may be a draft (created, editable, not sent), a
-- scheduled send (a `scheduled_at` time the in-process scheduler acts on), or
-- one of the terminal/active states it already had (sending/sent/failed), plus
-- an explicit canceled state for planned campaigns that are called off.

ALTER TABLE campaigns ADD COLUMN scheduled_at TIMESTAMPTZ;

-- Broaden the status domain. The constraint was created inline and unnamed, so
-- Postgres auto-named it `campaigns_status_check`.
ALTER TABLE campaigns DROP CONSTRAINT campaigns_status_check;
ALTER TABLE campaigns ADD CONSTRAINT campaigns_status_check
    CHECK (status IN ('draft', 'scheduled', 'sending', 'sent', 'failed', 'canceled'));

-- New campaigns default to draft (planned, not sent). The send-now and
-- scheduled paths set their own status explicitly on insert.
ALTER TABLE campaigns ALTER COLUMN status SET DEFAULT 'draft';

-- The scheduler claims due campaigns by (status, scheduled_at); index it.
CREATE INDEX idx_campaigns_due ON campaigns (status, scheduled_at);
