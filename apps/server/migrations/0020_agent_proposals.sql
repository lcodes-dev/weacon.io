-- Who proposed a change to the docs, when an agent did: which agent, the person it works for,
-- and its chat, which the team's own agent has none of. All NULL for the docs writer's.
ALTER TABLE proposals ADD COLUMN agent TEXT;
ALTER TABLE proposals ADD COLUMN user_id TEXT REFERENCES users (id) ON DELETE SET NULL;
ALTER TABLE proposals ADD COLUMN chat_id TEXT REFERENCES chats (id) ON DELETE SET NULL;

-- For the cascades from a deleted person and a deleted chat.
CREATE INDEX proposals_user ON proposals (user_id);
CREATE INDEX proposals_chat ON proposals (chat_id);
