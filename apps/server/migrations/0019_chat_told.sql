-- The newest message of its group that a chat's agent was told, so that its next call tells what came after.
ALTER TABLE chats ADD COLUMN told TEXT;

-- A chat called before this was kept has been told everything written so far.
UPDATE chats SET told = (SELECT MAX(id) FROM group_messages WHERE group_id = chats.group_id)
WHERE group_id IS NOT NULL;
