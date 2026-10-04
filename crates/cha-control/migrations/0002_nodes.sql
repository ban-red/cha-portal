-- Nodes and their enrollment (Phase 1, P1.2). Times are Unix seconds.

-- One-time tokens an admin hands to a new node; only their SHA-256 is stored.
CREATE TABLE join_tokens (
    token_hash TEXT PRIMARY KEY,
    label      TEXT,
    created_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    used_at    INTEGER,
    node_id    TEXT
);

-- A node is known by its Ed25519 public key; it proves it holds the key on
-- every connection. Deleting the row revokes the node.
CREATE TABLE nodes (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    public_key    TEXT NOT NULL UNIQUE,
    agent_version TEXT,
    enrolled_at   INTEGER NOT NULL,
    last_seen_at  INTEGER,
    -- JSON (cha_wire::Inventory), refreshed by the node.
    inventory     TEXT
);
