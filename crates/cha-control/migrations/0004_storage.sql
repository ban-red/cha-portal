-- App data settings (Phase 3, persistent environments). Which apps keep their
-- data is the user's choice, with a default per app; what apps share across
-- users is the admin's. Both start from the catalog (images/catalog.json): a
-- NULL here means "whatever the catalog says", so catalog edits reach
-- everything nobody has chosen for.

-- Per app, set by an admin.
CREATE TABLE app_storage (
    template_id        TEXT PRIMARY KEY,
    -- Whether users who haven't chosen keep their data for the app.
    default_persistent INTEGER,
    -- What the app shares across users: none (not mounted), read or write.
    shared_access      TEXT CHECK (shared_access IN ('none', 'read', 'write'))
);

-- A user's own choice for an app, once they've made one.
CREATE TABLE user_app_storage (
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    template_id TEXT NOT NULL,
    persistent  INTEGER NOT NULL,
    PRIMARY KEY (user_id, template_id)
);
