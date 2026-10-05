-- Which virtual controller an app gets (docs/controllers.md): the user's own
-- choice for an app, once they've made one. A user with none gets the app's
-- default from the catalog (images/catalog.json), so catalog edits reach
-- everyone who hasn't chosen. `kind` is a cha_wire::GamepadKind's name.
CREATE TABLE user_app_gamepad (
    user_id     TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    template_id TEXT NOT NULL,
    kind        TEXT NOT NULL CHECK (kind IN ('xbox360', 'dualsense', 'steam')),
    PRIMARY KEY (user_id, template_id)
);
