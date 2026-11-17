CREATE TABLE users (
    id INTEGER PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    -- The server's owner: the first account.
    is_owner INTEGER NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE teams (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE team_members (
    team_id INTEGER NOT NULL REFERENCES teams (id),
    user_id INTEGER NOT NULL REFERENCES users (id),
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    PRIMARY KEY (team_id, user_id)
) STRICT;

-- One live code per email. A name means the request was a registration.
CREATE TABLE sign_in_codes (
    email TEXT PRIMARY KEY,
    code TEXT NOT NULL,
    name TEXT,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES users (id),
    created_at INTEGER NOT NULL
) STRICT;

-- One row. SMTP is set when smtp_host is.
CREATE TABLE config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    smtp_host TEXT,
    smtp_port INTEGER,
    smtp_security TEXT CHECK (smtp_security IN ('tls', 'starttls', 'none')),
    smtp_username TEXT,
    smtp_password TEXT,
    mail_from TEXT
) STRICT;

INSERT INTO config (id) VALUES (1);
