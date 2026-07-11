-- Billing for the hosted cloud offering: the Stripe customer attached to
-- an organization. NULL for every organization on self-hosted
-- installations (the `billing` config group is disabled by default and no
-- billing endpoint ever writes it there). A config column, not tenant
-- data, so no RLS.
ALTER TABLE organizations ADD COLUMN billing_customer_id TEXT;
