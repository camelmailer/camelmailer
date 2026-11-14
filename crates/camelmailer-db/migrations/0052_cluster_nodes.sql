-- The cluster topology: every web, SMTP and worker process registers itself
-- here at start and refreshes last_seen_at on a heartbeat
-- (cluster.heartbeat_interval_seconds). instance_id is unique per process
-- and role; for a worker it is the id it locks queue rows with
-- (queued_messages.locked_by), so the dashboard can show which node holds
-- which leases. A worker that shuts down gracefully sets stopped_at; a
-- process that disappears without doing so goes stale once last_seen_at is
-- older than cluster.node_stale_seconds. Rows not seen for seven days are
-- removed by the processes themselves. Read by GET /api/v2/admin/cluster.

CREATE TABLE cluster_nodes (
    instance_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('web', 'smtp', 'worker')),
    node_name TEXT NOT NULL,
    version TEXT NOT NULL,
    details JSONB NOT NULL DEFAULT '{}'::jsonb,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    stopped_at TIMESTAMPTZ,
    PRIMARY KEY (instance_id, role)
);

CREATE INDEX cluster_nodes_last_seen_at ON cluster_nodes (last_seen_at);
