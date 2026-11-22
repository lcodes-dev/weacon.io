-- A person's conversation with an agent on one of their machines, about a project.
CREATE TABLE chats (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- NULL once the machine was removed: the chat can be read and not gone on with.
    machine_id TEXT REFERENCES machines (id) ON DELETE SET NULL,
    -- claude or codex
    agent TEXT NOT NULL,
    title TEXT NOT NULL,
    -- The branch the chat's folder on the machine started from.
    branch TEXT NOT NULL,
    -- What the person last wrote, which the running turn is about.
    asked TEXT NOT NULL,
    -- How the agent's program knows the conversation, once a turn has ended well.
    session TEXT,
    running INTEGER NOT NULL DEFAULT 1,
    -- How many of the machine's events for this chat the server has.
    heard INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX chats_project ON chats (project_id, id);
CREATE INDEX chats_machine ON chats (machine_id, running);
CREATE INDEX chats_user ON chats (user_id);

-- What was written and done in a chat. An id starts with its time, so they sort as they happened.
CREATE TABLE chat_events (
    id TEXT NOT NULL PRIMARY KEY,
    chat_id TEXT NOT NULL REFERENCES chats (id) ON DELETE CASCADE,
    -- JSON with a `kind`: asked, said, did, done or failed.
    body TEXT NOT NULL
) STRICT;

CREATE INDEX chat_events_chat ON chat_events (chat_id, id);
