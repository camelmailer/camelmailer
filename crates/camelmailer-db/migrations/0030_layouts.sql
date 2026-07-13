-- Reusable layouts: wrapper HTML (and optionally text) around a
-- template's rendered body — logos, postal addresses, social links that
-- every mail of a server shares. Config records like templates: filtered
-- by server_id, no RLS. Templates reference a layout loosely; deleting a
-- layout unhooks its templates instead of deleting them.
CREATE TABLE layouts (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    permalink TEXT NOT NULL,
    html_wrapper TEXT NOT NULL,
    text_wrapper TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT layouts_server_permalink_key UNIQUE (server_id, permalink)
);

ALTER TABLE templates
    ADD COLUMN layout_id BIGINT REFERENCES layouts(id) ON DELETE SET NULL;
