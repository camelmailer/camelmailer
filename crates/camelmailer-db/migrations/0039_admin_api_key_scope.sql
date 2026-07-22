-- Admin API keys can be scoped to an organization or, more narrowly, to a
-- single server. A scoped key may only act inside its subtree — platform
-- integrations get one machine key per tenant server instead of sharing the
-- installation-wide key. NULL scope keeps today's behaviour (full access).
ALTER TABLE admin_api_keys
    ADD COLUMN organization_id BIGINT REFERENCES organizations(id) ON DELETE CASCADE;
ALTER TABLE admin_api_keys
    ADD COLUMN server_id BIGINT REFERENCES servers(id) ON DELETE CASCADE;
