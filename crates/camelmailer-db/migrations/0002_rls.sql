-- Row-level security for tenant (mail-server) isolation on the shared
-- messages table.
--
-- Every message access must happen inside a transaction that establishes
-- the tenant context first:
--
--     SELECT set_config('camelmailer.server_id', '<server id>', true);
--
-- Without the context no rows are visible and no rows can be written; with
-- it, only the tenant's own rows are. FORCE makes the policy apply to the
-- table owner as well, so there is no application-level code path that can
-- cross tenants — only a superuser or an explicitly BYPASSRLS role
-- (e.g. a future cross-tenant queue worker) can.

ALTER TABLE messages ENABLE ROW LEVEL SECURITY;
ALTER TABLE messages FORCE ROW LEVEL SECURITY;

CREATE POLICY messages_tenant_isolation ON messages
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);
