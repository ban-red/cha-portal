-- Moonlight hosts (Sunshine, Apollo) an admin adopted through a node
-- (ADR 0008). The node found and paired with the host; the portal keeps what
-- launching it needs: where it was last seen, what it encodes, and the apps it
-- offered when last asked. An app launches as the template
-- `moonlight:<id>:<app id>`. Times are Unix seconds.
CREATE TABLE moonlight_hosts (
    id         TEXT PRIMARY KEY,
    node_id    TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    -- The host's own `uniqueid`, as the node writes it.
    unique_id  TEXT NOT NULL,
    name       TEXT NOT NULL,
    address    TEXT,
    http_port  INTEGER,
    https_port INTEGER,
    -- JSON arrays: ["h264", "hevc"], and [{"id": 1, "name": "Desktop", "hdr": false}].
    codecs     TEXT,
    apps       TEXT,
    apps_at    INTEGER,
    adopted_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER,
    UNIQUE (node_id, unique_id)
);
