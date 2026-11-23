-- Everything the chat changed in its folder, as its last turn left it. Empty when too long to show.
ALTER TABLE chats ADD COLUMN diff TEXT NOT NULL DEFAULT '';
-- The pull request the chat opened, once it has.
ALTER TABLE chats ADD COLUMN pull_number INTEGER;
ALTER TABLE chats ADD COLUMN pull_url TEXT;
