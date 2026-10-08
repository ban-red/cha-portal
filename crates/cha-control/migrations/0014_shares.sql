-- Share links for players (ADR 0014; the contract is docs/plans/share-links.md).
-- A share is a link, `/s/<token>`, that lets someone without an account join a
-- running environment with one role (`player` for now) on one gamepad slot
-- (1 to 3 is player 2 to 4). Only the token's SHA-256 (lower-case hex) is kept;
-- the token itself is shown once, when the share is made. A share ends when it
-- expires, is revoked, or its environment leaves `running` (`revoked_at`).
-- Times are Unix seconds.
CREATE TABLE shares (
    id             TEXT PRIMARY KEY,
    environment_id TEXT NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    created_by     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role           TEXT NOT NULL CHECK (role = 'player'),
    slot           INTEGER NOT NULL CHECK (slot BETWEEN 1 AND 3),
    token_hash     TEXT NOT NULL UNIQUE,
    created_at     INTEGER NOT NULL,
    expires_at     INTEGER NOT NULL,
    revoked_at     INTEGER
);

-- At most one live share per slot of an environment: making a new one revokes
-- the old.
CREATE UNIQUE INDEX shares_live_slot ON shares (environment_id, slot) WHERE revoked_at IS NULL;
CREATE INDEX shares_environment ON shares (environment_id);
