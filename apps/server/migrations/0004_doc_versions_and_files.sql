-- Every text a docs page has had. The newest is the page, and is also kept on its row in docs.
CREATE TABLE doc_versions (
    id TEXT NOT NULL PRIMARY KEY,
    doc_id TEXT NOT NULL REFERENCES docs (id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    markdown TEXT NOT NULL,
    -- Who wrote it, or NULL for the docs writer.
    author TEXT REFERENCES users (id),
    created_at INTEGER NOT NULL
) STRICT;

-- An id starts with its time, so a page's newest version is its greatest.
CREATE INDEX doc_versions_doc ON doc_versions (doc_id, id);

INSERT INTO doc_versions (id, doc_id, title, markdown, created_at)
SELECT id, id, title, markdown, unixepoch() FROM docs;

-- A file uploaded for a project's docs: an image a page shows, or an attachment it links to.
-- Its bytes are a file named by the id, in the folder beside the database.
CREATE TABLE files (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- An image's media type, such as image/png. Anything else is application/octet-stream.
    kind TEXT NOT NULL,
    -- What a download of it is called.
    name TEXT NOT NULL,
    created_by TEXT NOT NULL REFERENCES users (id),
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX files_project ON files (project_id);
