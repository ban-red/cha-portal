-- Accounts, browser sessions, audit log and portal settings (Phase 1, P1.1).
-- Times are Unix seconds.

CREATE TABLE users (
    id            TEXT PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    display_name  TEXT NOT NULL,
    -- Argon2id PHC string; NULL for accounts that only use passkeys (later).
    password_hash TEXT,
    role          TEXT NOT NULL CHECK (role IN ('admin', 'user', 'guest')),
    disabled      INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

-- Only a SHA-256 of each session token is stored.
CREATE TABLE sessions (
    token_hash   TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    user_agent   TEXT,
    ip           TEXT
);
CREATE INDEX sessions_user ON sessions (user_id);

-- Append-only: logins, launches, connects, shares, admin actions.
CREATE TABLE audit_log (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    at       INTEGER NOT NULL,
    actor_id TEXT,
    action   TEXT NOT NULL,
    target   TEXT,
    detail   TEXT,
    ip       TEXT
);
CREATE INDEX audit_at ON audit_log (at);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
