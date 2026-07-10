-- Virus-scanning result columns on messages (spam columns already exist
-- from migration 0005). Mirrors the inspection fields Postal records.

ALTER TABLE messages
    ADD COLUMN threat BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN threat_details TEXT,
    ADD COLUMN inspected BOOLEAN NOT NULL DEFAULT FALSE;
