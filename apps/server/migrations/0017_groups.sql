-- Some members of a team and what they write to each other. A direct chat is a group of two.
CREATE TABLE groups (
    id TEXT NOT NULL PRIMARY KEY,
    team_id TEXT NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    -- NULL for one called by its other members' names.
    name TEXT,
    -- The project it is about, given once, if it has one.
    project_id TEXT REFERENCES projects (id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX groups_team ON groups (team_id);
CREATE INDEX groups_project ON groups (project_id);

CREATE TABLE group_members (
    group_id TEXT NOT NULL REFERENCES groups (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- The newest message the member has read, or written.
    read TEXT,
    PRIMARY KEY (group_id, user_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX group_members_user ON group_members (user_id);

-- An id starts with its time, so a group's messages sort as they were written.
CREATE TABLE group_messages (
    id TEXT NOT NULL PRIMARY KEY,
    group_id TEXT NOT NULL REFERENCES groups (id) ON DELETE CASCADE,
    user_id TEXT REFERENCES users (id) ON DELETE SET NULL,
    -- Markdown.
    text TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX group_messages_group ON group_messages (group_id, id);
