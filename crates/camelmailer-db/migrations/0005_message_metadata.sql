-- Message metadata parity with `lib/postal/message_db/`: delivery status,
-- spam columns, parsed header fields, plus the per-message activity tables
-- (deliveries, links/clicks, loads/opens). Every activity table is
-- tenant-scoped and carries the same FORCE RLS policy as messages.

ALTER TABLE messages
    ADD COLUMN status TEXT NOT NULL DEFAULT 'Pending'
        CHECK (status IN ('Pending', 'Sent', 'SoftFail', 'HardFail', 'Held', 'Bounced')),
    ADD COLUMN subject TEXT,
    ADD COLUMN message_id_header TEXT,
    ADD COLUMN spam_status TEXT NOT NULL DEFAULT 'NotChecked'
        CHECK (spam_status IN ('NotChecked', 'NotSpam', 'Spam', 'SpamFailure')),
    ADD COLUMN spam_score DOUBLE PRECISION NOT NULL DEFAULT 0,
    ADD COLUMN held BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN size BIGINT NOT NULL DEFAULT 0,
    ADD COLUMN last_delivery_attempt TIMESTAMPTZ;

CREATE TABLE deliveries (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    message_id BIGINT NOT NULL,
    status TEXT NOT NULL
        CHECK (status IN ('Sent', 'SoftFail', 'HardFail', 'Held', 'Bounced')),
    details TEXT,
    output TEXT,
    sent_with_ssl BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_deliveries_message ON deliveries (server_id, message_id);

CREATE TABLE links (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    message_id BIGINT NOT NULL,
    token TEXT NOT NULL,
    url TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_links_message ON links (server_id, message_id);
CREATE INDEX idx_links_token ON links (token);

CREATE TABLE link_clicks (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    link_id BIGINT NOT NULL REFERENCES links(id) ON DELETE CASCADE,
    ip_address TEXT,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- "loads" = open-tracking pixel hits, keeping Postal's table name
CREATE TABLE loads (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    message_id BIGINT NOT NULL,
    ip_address TEXT,
    user_agent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_loads_message ON loads (server_id, message_id);

ALTER TABLE deliveries ENABLE ROW LEVEL SECURITY;
ALTER TABLE deliveries FORCE ROW LEVEL SECURITY;
CREATE POLICY deliveries_tenant_isolation ON deliveries
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);

ALTER TABLE links ENABLE ROW LEVEL SECURITY;
ALTER TABLE links FORCE ROW LEVEL SECURITY;
CREATE POLICY links_tenant_isolation ON links
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);

ALTER TABLE link_clicks ENABLE ROW LEVEL SECURITY;
ALTER TABLE link_clicks FORCE ROW LEVEL SECURITY;
CREATE POLICY link_clicks_tenant_isolation ON link_clicks
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);

ALTER TABLE loads ENABLE ROW LEVEL SECURITY;
ALTER TABLE loads FORCE ROW LEVEL SECURITY;
CREATE POLICY loads_tenant_isolation ON loads
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);
