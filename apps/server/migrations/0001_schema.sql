-- A row with an id of its own has a UUIDv7, made by the server. Other tables are keyed by what already names the row.

CREATE TABLE users (
    id TEXT NOT NULL PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    -- The server's owner: the first account.
    is_owner INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE teams (
    id TEXT NOT NULL PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    -- The team's own GitHub App. All three are NULL until the team's owner registers it.
    github_app_id INTEGER,
    github_app_slug TEXT,
    github_private_key TEXT,
    -- Who writes the team's docs. All three are NULL until the team's owner chooses.
    writer TEXT CHECK (writer IN ('claude_api', 'claude_code', 'codex')),
    writer_key TEXT,
    writer_model TEXT
) STRICT;

CREATE TABLE team_members (
    team_id TEXT NOT NULL REFERENCES teams (id),
    user_id TEXT NOT NULL REFERENCES users (id),
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    PRIMARY KEY (team_id, user_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX team_members_user ON team_members (user_id);

-- One live code per email. A name means the request was a registration.
CREATE TABLE sign_in_codes (
    email TEXT NOT NULL PRIMARY KEY,
    code TEXT NOT NULL,
    name TEXT,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE sessions (
    token TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users (id),
    created_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

-- One row. SMTP is set when smtp_host is.
CREATE TABLE config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    smtp_host TEXT,
    smtp_port INTEGER,
    smtp_security TEXT CHECK (smtp_security IN ('tls', 'starttls', 'none')),
    smtp_username TEXT,
    smtp_password TEXT,
    mail_from TEXT,
    -- Which version of the reader made the maps; a newer reader reads everything again.
    map_reader INTEGER NOT NULL DEFAULT 0
) STRICT;

INSERT INTO config (id) VALUES (1);

-- One per team and email. The token is the secret in the mailed link.
CREATE TABLE invites (
    id TEXT NOT NULL PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    email TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    invited_by TEXT NOT NULL REFERENCES users (id),
    expires_at INTEGER NOT NULL,
    UNIQUE (team_id, email)
) STRICT;

CREATE TABLE projects (
    id TEXT NOT NULL PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id),
    name TEXT NOT NULL,
    -- owner/name on GitHub, or NULL for a project with no repository.
    repo TEXT,
    created_at INTEGER NOT NULL,
    docs_state TEXT NOT NULL DEFAULT 'idle' CHECK (docs_state IN ('idle', 'running', 'failed')),
    -- Where a run is, or why it failed.
    docs_message TEXT,
    -- The run now writing the project's docs. A run that no longer holds it saves nothing.
    docs_run INTEGER,
    -- What the docs were written from.
    docs_branch TEXT,
    docs_commit TEXT,
    -- A JSON list of the folders the writer judged not to be the project's own code.
    docs_left_out TEXT
) STRICT;

CREATE INDEX projects_team ON projects (team_id);

-- A branch the project tracks. It waits to be mapped while head_sha differs from commit_sha.
CREATE TABLE branches (
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    -- The latest commit GitHub has.
    head_sha TEXT,
    -- The commit the map describes.
    commit_sha TEXT,
    error TEXT,
    PRIMARY KEY (project_id, name)
) STRICT, WITHOUT ROWID;

-- The files of a branch. blob is git's id of a file's content.
CREATE TABLE branch_files (
    project_id TEXT NOT NULL,
    branch TEXT NOT NULL,
    path TEXT NOT NULL,
    blob TEXT NOT NULL,
    PRIMARY KEY (project_id, branch, path),
    FOREIGN KEY (project_id, branch) REFERENCES branches (project_id, name) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- One row per content read for the project, whichever commits and branches have it.
CREATE TABLE blobs (
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    blob TEXT NOT NULL,
    lines INTEGER NOT NULL,
    -- A text file's content, which the writer reads. NULL for a file the map only lists.
    text TEXT,
    PRIMARY KEY (project_id, blob)
) STRICT;

CREATE TABLE blob_symbols (
    project_id TEXT NOT NULL,
    blob TEXT NOT NULL,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    PRIMARY KEY (project_id, blob, name, kind, line),
    FOREIGN KEY (project_id, blob) REFERENCES blobs (project_id, blob) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- The names a content mentions without defining them.
CREATE TABLE blob_mentions (
    project_id TEXT NOT NULL,
    blob TEXT NOT NULL,
    name TEXT NOT NULL,
    PRIMARY KEY (project_id, blob, name),
    FOREIGN KEY (project_id, blob) REFERENCES blobs (project_id, blob) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- The pages a writer generated from a project's code.
CREATE TABLE docs (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- works: what a user does and gets. built: how the project's own code works.
    section TEXT NOT NULL CHECK (section IN ('overview', 'works', 'built')),
    position INTEGER NOT NULL,
    title TEXT NOT NULL,
    markdown TEXT NOT NULL
) STRICT;

CREATE INDEX docs_project ON docs (project_id, position);
