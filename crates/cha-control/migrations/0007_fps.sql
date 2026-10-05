-- The frame rate an app runs at: the user's own choice for an app, once
-- they've made one. A user with none gets the app's default from the catalog
-- (images/catalog.json, 60 when it says nothing), so catalog edits reach
-- everyone who hasn't chosen.
CREATE TABLE user_app_fps (
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    template_id TEXT NOT NULL,
    fps         INTEGER NOT NULL CHECK (fps IN (60, 90, 120)),
    PRIMARY KEY (user_id, template_id)
);
