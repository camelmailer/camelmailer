-- Per-message metadata for the HTTP send API (Postmark-style Metadata),
-- stored as JSONB on the RLS-protected messages table.

ALTER TABLE messages ADD COLUMN metadata JSONB;
