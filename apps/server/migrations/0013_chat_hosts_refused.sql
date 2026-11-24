-- No agent waits to hear about a host any more: both are refused, and told again in a next turn.
ALTER TABLE chats DROP COLUMN asking;
ALTER TABLE chats DROP COLUMN asking_host;
