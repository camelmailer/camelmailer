-- Webhook trigger granularity + custom delivery headers.
--
-- `events` is the list of subscribed event names (empty = all events,
-- backwards compatible with existing rows); `headers` is a string map of
-- extra HTTP headers set on every delivery request (values are secrets —
-- they are stored, but never logged). `webhook_requests` snapshots the
-- headers at enqueue time, like it already snapshots the URL.

ALTER TABLE webhooks
    ADD COLUMN events JSONB NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN headers JSONB NOT NULL DEFAULT '{}'::jsonb;

ALTER TABLE webhook_requests
    ADD COLUMN headers JSONB NOT NULL DEFAULT '{}'::jsonb;
