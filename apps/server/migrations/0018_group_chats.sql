-- The group a chat's agent answers in, when its owner called it there. Such a chat goes with its group.
ALTER TABLE chats ADD COLUMN group_id TEXT REFERENCES groups (id) ON DELETE CASCADE;

CREATE INDEX chats_group ON chats (group_id, user_id);

-- An agent's answer in a group: which agent, for the person in `user_id`, and the chat it worked in.
ALTER TABLE group_messages ADD COLUMN agent TEXT;
ALTER TABLE group_messages ADD COLUMN chat_id TEXT REFERENCES chats (id) ON DELETE SET NULL;

CREATE INDEX group_messages_chat ON group_messages (chat_id);
