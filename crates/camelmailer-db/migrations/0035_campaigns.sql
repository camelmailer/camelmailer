-- Broadcast campaigns as a first-class entity. A campaign is a tracked send
-- of one piece of content to a broadcast stream's subscribers. The API
-- records the campaign, then a background task expands it into one message
-- per recipient (each carrying `campaign_id`) so per-campaign analytics can be
-- aggregated over messages/loads/link_clicks/suppressions.
--
-- Tenant data: shares the messages/suppressions/subscriptions RLS model.

CREATE TABLE campaigns (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    stream_id BIGINT NOT NULL REFERENCES message_streams(id) ON DELETE CASCADE,
    name TEXT,
    subject TEXT,
    from_address TEXT,
    html_body TEXT,
    text_body TEXT,
    status TEXT NOT NULL DEFAULT 'sending'
        CHECK (status IN ('sending', 'sent', 'failed')),
    total INT NOT NULL DEFAULT 0,
    sent INT NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ DEFAULT now(),
    completed_at TIMESTAMPTZ
);
CREATE INDEX idx_campaigns_stream ON campaigns (server_id, stream_id);

ALTER TABLE campaigns ENABLE ROW LEVEL SECURITY;
ALTER TABLE campaigns FORCE ROW LEVEL SECURITY;

CREATE POLICY campaigns_tenant_isolation ON campaigns
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);

-- Attribute a message to the campaign that produced it (NULL for one-off
-- sends). RLS on `messages` already scopes reads/writes to the tenant.
ALTER TABLE messages ADD COLUMN campaign_id BIGINT;
CREATE INDEX idx_messages_campaign ON messages (server_id, campaign_id);
