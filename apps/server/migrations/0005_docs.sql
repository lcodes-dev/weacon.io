-- The pages a writer generated from a project's code.
CREATE TABLE docs (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- works: what a user does and gets. built: how the project's own code works.
    section TEXT NOT NULL CHECK (section IN ('overview', 'works', 'built')),
    position INTEGER NOT NULL,
    title TEXT NOT NULL,
    markdown TEXT NOT NULL
) STRICT;

-- A text file's content, which the writer reads. NULL for a file the map only lists.
ALTER TABLE blobs ADD COLUMN text TEXT;

-- Who writes the team's docs. All three are NULL until the team's owner chooses.
ALTER TABLE teams ADD COLUMN writer TEXT CHECK (writer IN ('claude_api', 'claude_code', 'codex'));
ALTER TABLE teams ADD COLUMN writer_key TEXT;
ALTER TABLE teams ADD COLUMN writer_model TEXT;

ALTER TABLE projects ADD COLUMN docs_state TEXT NOT NULL DEFAULT 'idle'
    CHECK (docs_state IN ('idle', 'running', 'failed'));
-- Where a run is, or why it failed.
ALTER TABLE projects ADD COLUMN docs_message TEXT;
-- What the docs were written from.
ALTER TABLE projects ADD COLUMN docs_branch TEXT;
ALTER TABLE projects ADD COLUMN docs_commit TEXT;
-- A JSON list of the folders the writer judged not to be the project's own code.
ALTER TABLE projects ADD COLUMN docs_left_out TEXT;
