-- Moonlight clients paired with a node's GameStream host (ADR 0009). The
-- portal owns the list: a client pairs by showing a PIN that a signed-in user
-- types into the portal, and that user is the device's owner, whose
-- environments on the node it then sees. The node keeps a copy it is sent on
-- every connect and after every change. A client is named by the SHA-256 of
-- its certificate (lower-case hex); `unique_id` is only what it called
-- itself. Times are Unix seconds.
CREATE TABLE gamestream_devices (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    node_id     TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    unique_id   TEXT,
    name        TEXT NOT NULL,
    paired_at   INTEGER NOT NULL,
    UNIQUE (node_id, fingerprint)
);
CREATE INDEX gamestream_devices_user ON gamestream_devices (user_id);
