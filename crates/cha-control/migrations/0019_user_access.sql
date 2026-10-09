-- Local users by email, per-user limits and node restrictions, grants, and
-- "switch user" sessions.

ALTER TABLE users ADD COLUMN email TEXT;
CREATE UNIQUE INDEX users_email ON users (email COLLATE NOCASE) WHERE email IS NOT NULL;
-- NULL: the portal default.
ALTER TABLE users ADD COLUMN max_instances INTEGER;
-- A restricted user launches only on the nodes in user_nodes (and what
-- user_grants adds).
ALTER TABLE users ADD COLUMN node_restricted INTEGER NOT NULL DEFAULT 0;

-- Set while an admin is viewing as the session's user.
ALTER TABLE sessions ADD COLUMN impersonator_id TEXT REFERENCES users (id) ON DELETE CASCADE;

CREATE TABLE user_nodes (
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    node_id TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, node_id)
);

-- One template on one node, for a user who may not use the node otherwise.
CREATE TABLE user_grants (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    node_id     TEXT NOT NULL REFERENCES nodes (id) ON DELETE CASCADE,
    template_id TEXT NOT NULL,
    created_by  TEXT,
    created_at  INTEGER NOT NULL,
    UNIQUE (user_id, node_id, template_id)
);
CREATE INDEX user_grants_user ON user_grants (user_id);
