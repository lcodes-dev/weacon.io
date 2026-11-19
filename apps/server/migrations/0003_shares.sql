-- A public link to a copy of a project's user docs. The token is the secret in the link.
CREATE TABLE shares (
    token TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- The project's name when it was shared.
    title TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users (id),
    created_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX shares_project ON shares (project_id);

CREATE TABLE share_pages (
    token TEXT NOT NULL REFERENCES shares (token) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    title TEXT NOT NULL,
    markdown TEXT NOT NULL,
    PRIMARY KEY (token, position)
) STRICT, WITHOUT ROWID;
