-- Cha Player sign-in (C2.1, ADR 0013). A device is one install of the native
-- player signed in as a user; it holds a long-lived bearer token (only its
-- SHA-256, lower-case hex, is kept here) that opens the player's routes and
-- nothing else. `install_id` is the random id the player made once, so signing
-- in again from the same install replaces the token on its row.
-- `last_used_at` and `last_ip` are refreshed at most once a minute.
CREATE TABLE devices (
    id           TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    install_id   TEXT NOT NULL,
    name         TEXT NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    created_at   INTEGER NOT NULL,
    last_used_at INTEGER NOT NULL,
    last_ip      TEXT,
    UNIQUE (user_id, install_id)
);
CREATE INDEX devices_user ON devices (user_id);

-- One-use links from a signed-in browser to the player (`cha://connect?...`),
-- 60 seconds. Keyed by the SHA-256 of the ticket.
CREATE TABLE device_tickets (
    ticket_hash TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at  INTEGER NOT NULL,
    expires_at  INTEGER NOT NULL
);

-- A player asking to be signed in by typing a code in the portal. The player
-- polls with the secret `device_code` (kept as its SHA-256); the user types the
-- short `user_code` (8 letters, no dash) and approves or denies. `user_id` is
-- set on approval. `last_poll_at` makes a player that polls too fast slow
-- down. Times are Unix seconds.
CREATE TABLE device_codes (
    code_hash    TEXT PRIMARY KEY,
    user_code    TEXT NOT NULL UNIQUE,
    install_id   TEXT NOT NULL,
    name         TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved', 'denied')),
    user_id      TEXT REFERENCES users (id) ON DELETE CASCADE,
    created_at   INTEGER NOT NULL,
    expires_at   INTEGER NOT NULL,
    last_poll_at INTEGER
);
