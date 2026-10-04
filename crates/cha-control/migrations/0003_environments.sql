-- Environments (Phase 1, P1.4): one app from the catalog, running on a node for
-- its owner. Ephemeral for now: stopping one destroys it. Times are Unix seconds.
CREATE TABLE environments (
    id          TEXT PRIMARY KEY,
    owner_id    TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    template_id TEXT NOT NULL,
    node_id     TEXT REFERENCES nodes (id) ON DELETE SET NULL,
    -- starting | running | stopping | destroyed | failed
    state       TEXT NOT NULL,
    -- Why it failed or ended, for people.
    detail      TEXT,
    -- Where its streamer listens on the node.
    http_port   INTEGER,
    webrtc_port INTEGER,
    created_at  INTEGER NOT NULL,
    updated_at  INTEGER NOT NULL
);

CREATE INDEX environments_by_owner ON environments (owner_id, created_at);
CREATE INDEX environments_by_node ON environments (node_id, state);
