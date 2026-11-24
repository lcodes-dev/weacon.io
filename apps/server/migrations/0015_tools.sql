-- What the agent of a chat's running turn shows the server to use its tools. Each turn gets a new one.
ALTER TABLE chats ADD COLUMN tools_token TEXT;

CREATE UNIQUE INDEX chats_tools_token ON chats (tools_token);

-- The chat whose agent made the task, if one did.
ALTER TABLE tasks ADD COLUMN chat_id TEXT REFERENCES chats (id) ON DELETE SET NULL;

CREATE INDEX tasks_chat ON tasks (chat_id);
