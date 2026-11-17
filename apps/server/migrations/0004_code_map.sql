-- A branch the project tracks. It waits to be mapped while head_sha differs from commit_sha.
CREATE TABLE branches (
    project_id INTEGER NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    -- The latest commit GitHub has.
    head_sha TEXT,
    -- The commit the map describes.
    commit_sha TEXT,
    error TEXT,
    PRIMARY KEY (project_id, name)
) STRICT;

-- The files of a branch. blob is git's id of a file's content.
CREATE TABLE branch_files (
    project_id INTEGER NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    branch TEXT NOT NULL,
    path TEXT NOT NULL,
    blob TEXT NOT NULL,
    PRIMARY KEY (project_id, branch, path)
) STRICT;

-- One row per content read for the project, whichever commits and branches have it.
CREATE TABLE blobs (
    project_id INTEGER NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    blob TEXT NOT NULL,
    -- NULL for a language the map doesn't read.
    language TEXT,
    lines INTEGER NOT NULL,
    PRIMARY KEY (project_id, blob)
) STRICT;

CREATE TABLE blob_symbols (
    project_id INTEGER NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    blob TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    line INTEGER NOT NULL,
    end_line INTEGER NOT NULL
) STRICT;

CREATE INDEX blob_symbols_blob ON blob_symbols (project_id, blob);

-- The names a content mentions without defining them.
CREATE TABLE blob_mentions (
    project_id INTEGER NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    blob TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (project_id, blob, name)
) STRICT;

-- Which version of the reader made the maps; a newer reader reads everything again.
ALTER TABLE config ADD COLUMN map_reader INTEGER NOT NULL DEFAULT 0;
