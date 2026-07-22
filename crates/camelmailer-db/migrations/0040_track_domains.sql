-- Per-server click/open tracking domains. When a server has a verified
-- track domain, tracking URLs in its outgoing mail use it instead of the
-- installation-wide `dns.track_domain`. Config records (like routes), not
-- under RLS; the domain must CNAME to this installation so the public
-- /track/* endpoints receive the hits.
CREATE TABLE track_domains (
    id BIGSERIAL PRIMARY KEY,
    uuid TEXT NOT NULL UNIQUE,
    server_id BIGINT NOT NULL REFERENCES servers(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    verified BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (server_id, name)
);
