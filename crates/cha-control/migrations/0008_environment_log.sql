-- What an environment's containers last logged when it died on its own (its
-- node removes them, so this is all that is left): a JSON array of lines, for
-- its owner and admins to read. NULL: none kept.
ALTER TABLE environments ADD COLUMN log TEXT;
