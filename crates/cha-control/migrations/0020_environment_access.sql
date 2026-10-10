-- Who besides its owner and the admins may join a running environment:
--   private  nobody else (the default)
--   public   any signed-in user may watch and ask for the controls or a pad
--   free     any signed-in user may watch and take the controls freely
-- Chosen per environment; a launch copies the owner's default (the
-- `sessionAccess` preference).
ALTER TABLE environments ADD COLUMN access TEXT NOT NULL DEFAULT 'private'
    CHECK (access IN ('private', 'public', 'free'));
