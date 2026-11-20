-- A person's own computer or virtual machine, on which their core runs agents for them.
CREATE TABLE machines (
    id TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    -- Its system, as the core names it: macos or linux.
    os TEXT NOT NULL,
    -- What its core connects with.
    token TEXT NOT NULL UNIQUE,
    -- What the core last said of its agents, as a JSON list.
    agents TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL,
    -- When its core was last connected.
    seen_at INTEGER
) STRICT;

CREATE INDEX machines_user ON machines (user_id, id);

-- A code that pairs one machine with the person who asked for it, once, within ten minutes.
CREATE TABLE machine_codes (
    code TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE INDEX machine_codes_user ON machine_codes (user_id);
