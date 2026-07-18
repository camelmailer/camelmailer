-- A layout may carry a logo image stored directly in Postgres (bytes +
-- content type). Served by a public endpoint so mails reference an absolute
-- URL that survives in real email clients (unlike inline data: URIs).
ALTER TABLE layouts ADD COLUMN logo BYTEA;
ALTER TABLE layouts ADD COLUMN logo_content_type TEXT;
