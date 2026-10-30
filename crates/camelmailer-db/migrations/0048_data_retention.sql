-- Per-organization data retention overrides and message content purging.
--
-- Each retention_* column overrides the installation default of the
-- `retention` config group for one data class. NULL inherits the default,
-- 0 keeps the data forever, a positive value keeps it that many days. Every
-- existing organization gets NULL, so this migration changes no behaviour.
--
-- messages.content_purged_at records when the worker emptied raw_message
-- under retention.message_content_days. The row itself (status, subject,
-- recipients, deliveries) stays until the metadata period ends.

ALTER TABLE organizations ADD COLUMN retention_message_content_days INTEGER
    CHECK (retention_message_content_days >= 0);
ALTER TABLE organizations ADD COLUMN retention_message_metadata_days INTEGER
    CHECK (retention_message_metadata_days >= 0);
ALTER TABLE organizations ADD COLUMN retention_webhook_log_days INTEGER
    CHECK (retention_webhook_log_days >= 0);
ALTER TABLE organizations ADD COLUMN retention_dmarc_report_days INTEGER
    CHECK (retention_dmarc_report_days >= 0);

ALTER TABLE messages ADD COLUMN content_purged_at TIMESTAMPTZ;

-- The cleanup job scans by age; these keep its batches cheap on large tables.
CREATE INDEX idx_messages_server_created ON messages (server_id, created_at);
CREATE INDEX idx_webhook_request_log_server_created ON webhook_request_log (server_id, created_at);
CREATE INDEX idx_auth_events_user ON auth_events (user_id);
