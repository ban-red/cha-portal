-- Custom environments (ADR 0021): a built-in or catalog template saved under
-- a new name, with only the fields it changes. The id is `custom.<slug>`. It
-- is resolved at launch (base, then overrides), so a base updated by a
-- catalog refresh reaches every field the custom one didn't change. Times are
-- Unix seconds.
CREATE TABLE custom_templates (
    id         TEXT PRIMARY KEY,
    -- The template id it was made from (built-in or `<catalog>.<app>`).
    base       TEXT NOT NULL,
    -- 1: it keeps its base's app data instead of its own.
    share_data INTEGER NOT NULL,
    -- The fields it overrides, as JSON (camelCase, all optional).
    overrides  TEXT NOT NULL,
    -- cha_wire::HostOptions as JSON, or NULL for none.
    host       TEXT,
    -- An uploaded SVG; NULL uses the base's icon.
    icon       BLOB,
    created_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    updated_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

-- The host ports published for a live environment's host options, as a JSON
-- array of cha_wire::HostPort with the node's port filled in; NULL for none.
ALTER TABLE environments ADD COLUMN ports TEXT;
