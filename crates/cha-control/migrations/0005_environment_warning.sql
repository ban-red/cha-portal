-- Something a node noticed about a running environment that its user should
-- know (the app's own folders came unmounted, say). Set and cleared by the
-- node while the environment runs; cleared when it ends. NULL: nothing to say.
ALTER TABLE environments ADD COLUMN warning TEXT;
