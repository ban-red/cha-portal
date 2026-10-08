-- Share links for viewers and controllers (ADR 0015; the contract is
-- docs/plans/share-links.md). `role` is now `player`, `viewer` or
-- `controller`, and `slot` is set for a player only (1 to 3 is player 2 to 4)
-- and null otherwise. SQLite can't alter a CHECK, so the table is rebuilt and
-- its rows kept (every existing one is a player). Nothing references `shares`.
CREATE TABLE shares_new (
    id             TEXT PRIMARY KEY,
    environment_id TEXT NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    created_by     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role           TEXT NOT NULL CHECK (role IN ('player', 'viewer', 'controller')),
    slot           INTEGER,
    token_hash     TEXT NOT NULL UNIQUE,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL,
    revoked_at     INTEGER,
    CHECK (
        (role = 'player' AND slot IS NOT NULL AND slot BETWEEN 1 AND 3)
        OR (role <> 'player' AND slot IS NULL)
    )
);

INSERT INTO shares_new (id, environment_id, created_by, role, slot, token_hash, created_at, expires_at, revoked_at)
SELECT id, environment_id, created_by, role, slot, token_hash, created_at, expires_at, revoked_at
FROM shares;

DROP TABLE shares;
ALTER TABLE shares_new RENAME TO shares;

-- At most one live link per player slot, and one live controller link per
-- environment: making a new one revokes the old. Viewer links are unlimited.
CREATE UNIQUE INDEX shares_live_slot ON shares (environment_id, slot)
    WHERE revoked_at IS NULL AND role = 'player';
CREATE UNIQUE INDEX shares_live_controller ON shares (environment_id)
    WHERE revoked_at IS NULL AND role = 'controller';
CREATE INDEX shares_environment ON shares (environment_id);
