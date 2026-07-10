-- Message templates: named subject/html/text bodies rendered with a
-- per-send model (Mustache subset). A config table like message_streams —
-- NOT RLS-protected — so every query filters on server_id explicitly.

CREATE TABLE templates (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    permalink TEXT NOT NULL,
    subject TEXT,
    html_body TEXT,
    text_body TEXT,
    archived BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, permalink)
);
CREATE INDEX idx_templates_server ON templates (server_id);
