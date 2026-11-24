-- A project's short name, which its tasks are named by, as SF-12.
ALTER TABLE projects ADD COLUMN key TEXT NOT NULL DEFAULT '';
UPDATE projects SET key = upper(substr(replace(name, ' ', ''), 1, 3));
-- How many tasks the project ever had, so that a deleted task's number isn't given again.
ALTER TABLE projects ADD COLUMN tasks_made INTEGER NOT NULL DEFAULT 0;

-- Something a project needs done.
CREATE TABLE tasks (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    title TEXT NOT NULL,
    -- Markdown.
    text TEXT NOT NULL,
    -- JSON: what it is done when, each a `text` and whether it is `done`.
    checks TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('todo', 'doing', 'done')),
    -- Where it stands among the project's tasks of its status, the smallest first.
    position INTEGER NOT NULL,
    -- Who does it, if anybody.
    owner_id TEXT REFERENCES users (id) ON DELETE SET NULL,
    -- Who asked for it.
    asker_id TEXT REFERENCES users (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX tasks_project ON tasks (project_id, status, position);

-- The pages of the project's docs a task touches.
CREATE TABLE task_docs (
    task_id TEXT NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    doc_id TEXT NOT NULL REFERENCES docs (id) ON DELETE CASCADE,
    PRIMARY KEY (task_id, doc_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX task_docs_doc ON task_docs (doc_id);

-- The task a chat is for, if any.
ALTER TABLE chats ADD COLUMN task_id TEXT REFERENCES tasks (id) ON DELETE SET NULL;

CREATE INDEX chats_task ON chats (task_id);
