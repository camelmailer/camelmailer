-- Provider feedback: dated snapshots of what Google Postmaster Tools,
-- Microsoft SNDS and the complaint feedback loops report, and the fetch
-- state of each source. Snapshots carry counts and rates only, never
-- recipient addresses.

CREATE TABLE feedback_snapshots (
    id BIGSERIAL PRIMARY KEY,
    source TEXT NOT NULL,
    -- a provider group name (`google`, `microsoft`, `united_internet`, ...);
    -- groups are configurable, so only the format is checked
    provider TEXT NOT NULL CHECK (provider ~ '^[a-z0-9_]+$'),
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('domain', 'ip', 'provider')),
    subject TEXT NOT NULL,
    data_date DATE NOT NULL,
    verdict TEXT NOT NULL CHECK (verdict IN ('unknown', 'ok', 'warn', 'bad')),
    detail TEXT NOT NULL DEFAULT '',
    metrics JSONB NOT NULL DEFAULT '{}'::jsonb,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT feedback_snapshots_key UNIQUE (source, provider, subject_kind, subject, data_date)
);

CREATE INDEX idx_feedback_snapshots_provider_subject
    ON feedback_snapshots (provider, subject_kind, subject, data_date DESC);

CREATE TABLE feedback_sources (
    source TEXT PRIMARY KEY,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    last_attempt_at TIMESTAMPTZ,
    last_success_at TIMESTAMPTZ,
    last_error TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ,
    lease_holder TEXT,
    lease_until TIMESTAMPTZ
);

-- Per mail server: suppress the complainant of transactional mail on a
-- feedback-loop complaint. NULL follows the installation default
-- (`deliverability_feedback.fbl.suppress_complainants`).
CREATE TABLE server_feedback_settings (
    server_id BIGINT PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    suppress_complainants BOOLEAN
);

-- The inbound messages that were ARF feedback reports. They hold recipient
-- data, so their raw content is removed after
-- `retention.feedback_report_days`; the counts above stay.
CREATE TABLE feedback_reports (
    server_id BIGINT NOT NULL,
    message_id BIGINT NOT NULL,
    received_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (server_id, message_id)
);

CREATE INDEX idx_feedback_reports_received_at ON feedback_reports (received_at);
