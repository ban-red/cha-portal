-- A user's cosmetic preferences (theme, appearance, contrast, motion,
-- transparency), kept as one small JSON object so they follow the user to
-- another device. A user with no row has chosen nothing and gets the
-- client's defaults. The server validates keys and values (src/prefs.rs);
-- new preferences are new keys, not new columns.
CREATE TABLE user_prefs (
    user_id    TEXT PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    prefs      TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
