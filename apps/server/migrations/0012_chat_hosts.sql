-- The hosts the chat's commands may reach, as a JSON array of exact names.
ALTER TABLE chats ADD COLUMN hosts TEXT NOT NULL DEFAULT '[]';
-- The id under which the chat's agent is waiting to hear whether it may reach a host, and that host.
ALTER TABLE chats ADD COLUMN asking TEXT;
ALTER TABLE chats ADD COLUMN asking_host TEXT;
