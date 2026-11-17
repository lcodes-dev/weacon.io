CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    team_id INTEGER NOT NULL REFERENCES teams (id),
    name TEXT NOT NULL,
    -- owner/name on GitHub, or NULL for a project with no repository.
    repo TEXT,
    created_at INTEGER NOT NULL
) STRICT;

-- The team's own GitHub App. All three are NULL until the team's owner registers it.
ALTER TABLE teams ADD COLUMN github_app_id INTEGER;
ALTER TABLE teams ADD COLUMN github_app_slug TEXT;
ALTER TABLE teams ADD COLUMN github_private_key TEXT;
