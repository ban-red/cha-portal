-- Catalogs an admin loads (ADR 0019). A catalog is a JSON document of
-- templates; its templates get the ids `<slug>.<local id>`. The last good
-- document is kept as it was fetched, so a failed refresh changes nothing.
-- Times are Unix seconds.
CREATE TABLE catalogs (
    slug          TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    -- Where a refresh fetches it; NULL for a document the admin pasted.
    url           TEXT,
    document      TEXT NOT NULL,
    added_by      TEXT REFERENCES users (id) ON DELETE SET NULL,
    added_at      INTEGER NOT NULL,
    fetched_at    INTEGER NOT NULL,
    last_error    TEXT,
    last_error_at INTEGER
);

-- An admin's approval of one template's elevated profile. It holds the
-- profile and the image's registry host as they were approved: a refresh that
-- changes either one deletes the row.
CREATE TABLE catalog_approvals (
    slug        TEXT NOT NULL REFERENCES catalogs (slug) ON DELETE CASCADE,
    app         TEXT NOT NULL,
    profile     TEXT NOT NULL,
    image_host  TEXT NOT NULL,
    approved_by TEXT REFERENCES users (id) ON DELETE SET NULL,
    approved_at INTEGER NOT NULL,
    PRIMARY KEY (slug, app)
);

-- The icon fetched when the catalog was loaded: SVG bytes, or why there are
-- none. `source` is the URL it came from, so a refresh that fails to fetch an
-- unchanged icon keeps the one it has.
CREATE TABLE catalog_icons (
    slug   TEXT NOT NULL REFERENCES catalogs (slug) ON DELETE CASCADE,
    app    TEXT NOT NULL,
    source TEXT NOT NULL,
    svg    BLOB,
    error  TEXT,
    PRIMARY KEY (slug, app)
);
