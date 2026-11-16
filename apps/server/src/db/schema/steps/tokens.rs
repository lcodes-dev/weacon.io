/// Session tokens, machine tokens, pairing codes and claim secrets as they are, not hashed.
/// Every session, machine and pairing is dropped, so people sign in and pair again.
pub(crate) const PLAIN_TOKENS: &str = "
DROP TABLE sessions;
DROP TABLE pairings;
DROP TABLE machines;
DELETE FROM shared_machines;

CREATE TABLE sessions (
    token TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE machines (
    machine_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    token TEXT NOT NULL UNIQUE,
    paired_at INTEGER NOT NULL,
    core_build TEXT,
    core_offers TEXT
) STRICT;

CREATE TABLE pairings (
    pairing_id TEXT PRIMARY KEY,
    code TEXT NOT NULL UNIQUE,
    secret TEXT NOT NULL,
    token TEXT NOT NULL,
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    account_id TEXT REFERENCES accounts (account_id),
    machine_id TEXT,
    confirmed_at INTEGER,
    claimed INTEGER NOT NULL DEFAULT 0
) STRICT;
";
