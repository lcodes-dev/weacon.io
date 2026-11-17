-- One per team and email. The token is the secret in the mailed link.
CREATE TABLE invites (
    id INTEGER PRIMARY KEY,
    team_id INTEGER NOT NULL REFERENCES teams (id),
    email TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    invited_by INTEGER NOT NULL REFERENCES users (id),
    expires_at INTEGER NOT NULL,
    UNIQUE (team_id, email)
) STRICT;
