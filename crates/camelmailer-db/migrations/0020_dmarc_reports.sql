-- DMARC aggregate-report storage (RUA ingestion).
--
-- Both tables are tenant data like `messages`: protected by row-level
-- security keyed on the same `camelmailer.server_id` transaction setting,
-- with FORCE so not even the table owner can cross tenants. The rows table
-- carries its own server_id (denormalized from the report) so the policy
-- needs no subquery.

-- Ingested reports mark their carrier message "Processed" (the inbound
-- terminal state Postal used); teach the status checks that value.
ALTER TABLE messages DROP CONSTRAINT messages_status_check;
ALTER TABLE messages ADD CONSTRAINT messages_status_check
    CHECK (status IN ('Pending', 'Sent', 'SoftFail', 'HardFail', 'Held', 'Bounced', 'Processed'));
ALTER TABLE deliveries DROP CONSTRAINT deliveries_status_check;
ALTER TABLE deliveries ADD CONSTRAINT deliveries_status_check
    CHECK (status IN ('Sent', 'SoftFail', 'HardFail', 'Held', 'Bounced', 'Processed'));

CREATE TABLE dmarc_reports (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    domain TEXT NOT NULL,
    org_name TEXT,
    org_email TEXT,
    report_id TEXT NOT NULL,
    date_range_begin TIMESTAMPTZ NOT NULL,
    date_range_end TIMESTAMPTZ NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_dmarc_reports_server_domain ON dmarc_reports (server_id, domain);
CREATE INDEX idx_dmarc_reports_range ON dmarc_reports (server_id, date_range_begin);

CREATE TABLE dmarc_report_records (
    id BIGSERIAL PRIMARY KEY,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    report_id BIGINT NOT NULL REFERENCES dmarc_reports(id) ON DELETE CASCADE,
    source_ip TEXT NOT NULL,
    count BIGINT NOT NULL,
    disposition TEXT NOT NULL,
    dkim_result TEXT,
    spf_result TEXT,
    dkim_aligned BOOLEAN NOT NULL,
    spf_aligned BOOLEAN NOT NULL,
    header_from TEXT,
    envelope_from TEXT
);
CREATE INDEX idx_dmarc_report_records_report ON dmarc_report_records (report_id);
CREATE INDEX idx_dmarc_report_records_server ON dmarc_report_records (server_id);

ALTER TABLE dmarc_reports ENABLE ROW LEVEL SECURITY;
ALTER TABLE dmarc_reports FORCE ROW LEVEL SECURITY;
CREATE POLICY dmarc_reports_tenant_isolation ON dmarc_reports
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);

ALTER TABLE dmarc_report_records ENABLE ROW LEVEL SECURITY;
ALTER TABLE dmarc_report_records FORCE ROW LEVEL SECURITY;
CREATE POLICY dmarc_report_records_tenant_isolation ON dmarc_report_records
    USING (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint)
    WITH CHECK (server_id = NULLIF(current_setting('camelmailer.server_id', true), '')::bigint);
