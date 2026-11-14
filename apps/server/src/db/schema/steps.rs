pub(crate) const WORKSPACES: &str = "
CREATE TABLE workspaces (
    workspace_id TEXT PRIMARY KEY,
    name TEXT NOT NULL
) STRICT;

CREATE TABLE members (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    member_id TEXT NOT NULL,
    email TEXT NOT NULL,
    PRIMARY KEY (workspace_id, member_id),
    UNIQUE (workspace_id, email)
) STRICT;
";

pub(crate) const SIGN_IN: &str = "
CREATE TABLE sign_in_codes (
    workspace_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    sent_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL,
    window_start INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, member_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE sessions (
    workspace_id TEXT NOT NULL,
    token_hash BLOB NOT NULL,
    member_id TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, token_hash),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE changes (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;
";

pub(crate) const CHALLENGES: &str = "
DROP TABLE sign_in_codes;

CREATE TABLE sign_in_challenges (
    workspace_id TEXT NOT NULL,
    challenge_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, challenge_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;
";

/// Account ids in the format of `app::new_id`.
pub(crate) const ACCOUNTS: &str = "
CREATE TABLE accounts (
    account_id TEXT PRIMARY KEY,
    email TEXT NOT NULL UNIQUE
) STRICT;

INSERT INTO accounts (account_id, email)
SELECT lower(hex(randomblob(16))), email FROM (SELECT DISTINCT email FROM members);

ALTER TABLE members ADD COLUMN account_id TEXT REFERENCES accounts (account_id);

UPDATE members SET account_id =
    (SELECT account_id FROM accounts WHERE accounts.email = members.email);
";

/// A person signs in to their account, not to one workspace.
pub(crate) const ACCOUNT_SIGN_IN: &str = "
CREATE UNIQUE INDEX members_account ON members (workspace_id, account_id);

DROP TABLE sign_in_challenges;
DROP TABLE sessions;

CREATE TABLE sign_in_challenges (
    challenge_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL
) STRICT;

CREATE TABLE sessions (
    token_hash BLOB PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    expires_at INTEGER NOT NULL
) STRICT;
";

/// Tasks, each with the versions it went through.
pub(crate) const TASKS: &str = "
CREATE TABLE tasks (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    task_id TEXT NOT NULL,
    title TEXT NOT NULL,
    status TEXT NOT NULL,
    order_key TEXT NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, task_id),
    UNIQUE (workspace_id, order_key)
) STRICT;

CREATE TABLE task_versions (
    workspace_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    author_kind TEXT NOT NULL,
    author_member TEXT NOT NULL,
    accepted_by TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, task_id) REFERENCES tasks (workspace_id, task_id),
    FOREIGN KEY (workspace_id, author_member) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, accepted_by) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE changes_with_tasks (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    task_id TEXT,
    task_version INTEGER,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, task_id, task_version)
        REFERENCES task_versions (workspace_id, task_id, version)
) STRICT;

INSERT INTO changes_with_tasks (workspace_id, seq, kind, member_id)
SELECT workspace_id, seq, kind, member_id FROM changes;

DROP TABLE changes;
ALTER TABLE changes_with_tasks RENAME TO changes;
";

/// What each member may do, and whether an account has had its Personal workspace made.
pub(crate) const ROLES: &str = "
ALTER TABLE members ADD COLUMN role TEXT NOT NULL DEFAULT 'member';
ALTER TABLE accounts ADD COLUMN personal_made INTEGER NOT NULL DEFAULT 0;
";

/// A challenge is for an address, whose account a right code may make. A server's owner is
/// its first account; one from before is that of the first member added.
pub(crate) const CHALLENGES_BY_EMAIL: &str = "
DROP TABLE sign_in_challenges;

CREATE TABLE sign_in_challenges (
    challenge_id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    code_hash BLOB NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL
) STRICT;

ALTER TABLE accounts ADD COLUMN owner INTEGER NOT NULL DEFAULT 0;
UPDATE accounts SET owner = 1 WHERE account_id = coalesce(
    (SELECT account_id FROM members ORDER BY rowid LIMIT 1),
    (SELECT account_id FROM accounts ORDER BY rowid LIMIT 1)
);
CREATE UNIQUE INDEX accounts_owner ON accounts (owner) WHERE owner = 1;
";

/// Only so many live codes are mailed to an address; its other challenges take those codes.
pub(crate) const MAILED: &str = "
ALTER TABLE sign_in_challenges ADD COLUMN mailed INTEGER NOT NULL DEFAULT 1;
CREATE INDEX sign_in_challenges_email ON sign_in_challenges (email);
";

/// Docs, each revision whole, with the pins it links to.
pub(crate) const DOCS: &str = "
CREATE TABLE docs (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    doc_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, doc_id)
) STRICT;

CREATE TABLE doc_versions (
    workspace_id TEXT NOT NULL,
    doc_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    commit_id TEXT,
    author_kind TEXT NOT NULL,
    author_member TEXT NOT NULL,
    accepted_by TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, doc_id, version),
    FOREIGN KEY (workspace_id, doc_id) REFERENCES docs (workspace_id, doc_id),
    FOREIGN KEY (workspace_id, author_member) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, accepted_by) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE TABLE doc_pins (
    workspace_id TEXT NOT NULL,
    doc_id TEXT NOT NULL,
    version INTEGER NOT NULL,
    position INTEGER NOT NULL,
    pin TEXT NOT NULL,
    PRIMARY KEY (workspace_id, doc_id, version, position),
    FOREIGN KEY (workspace_id, doc_id, version)
        REFERENCES doc_versions (workspace_id, doc_id, version)
) STRICT;

CREATE TABLE changes_with_docs (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    task_id TEXT,
    task_version INTEGER,
    doc_id TEXT,
    doc_version INTEGER,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, task_id, task_version)
        REFERENCES task_versions (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, doc_id, doc_version)
        REFERENCES doc_versions (workspace_id, doc_id, version)
) STRICT;

INSERT INTO changes_with_docs (workspace_id, seq, kind, member_id, task_id, task_version)
SELECT workspace_id, seq, kind, member_id, task_id, task_version FROM changes;

DROP TABLE changes;
ALTER TABLE changes_with_docs RENAME TO changes;
";

/// Machines paired with an account, and the pairings under way. A confirmed pairing names
/// its account and machine, and stays until it expires so that its core may claim again.
pub(crate) const MACHINES: &str = "
CREATE TABLE machines (
    machine_id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL REFERENCES accounts (account_id),
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    machine_key TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE,
    paired_at INTEGER NOT NULL
) STRICT;

CREATE TABLE pairings (
    pairing_id TEXT PRIMARY KEY,
    code_hash BLOB NOT NULL UNIQUE,
    secret_hash BLOB NOT NULL,
    name TEXT NOT NULL,
    os TEXT NOT NULL,
    machine_key TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    account_id TEXT REFERENCES accounts (account_id),
    machine_id TEXT,
    confirmed_at INTEGER,
    claimed INTEGER NOT NULL DEFAULT 0
) STRICT;
";

/// The bytes of doc revisions each workspace holds, which its quota bounds. Each revision
/// also counts 256 bytes for its rows, as `db::docs` counts them.
pub(crate) const DOC_BYTES: &str = "
ALTER TABLE workspaces ADD COLUMN doc_bytes INTEGER NOT NULL DEFAULT 0;

UPDATE workspaces SET doc_bytes =
    (SELECT COALESCE(SUM(256 + length(CAST(title AS BLOB)) + length(CAST(body AS BLOB))
         + COALESCE(length(commit_id), 0)), 0)
     FROM doc_versions WHERE doc_versions.workspace_id = workspaces.workspace_id)
    + (SELECT COALESCE(SUM(length(pin)), 0)
       FROM doc_pins WHERE doc_pins.workspace_id = workspaces.workspace_id);
";

/// Which agent made a version, as its JSON, when its author names one.
pub(crate) const VERSION_AGENTS: &str = "
ALTER TABLE task_versions ADD COLUMN agent TEXT;
ALTER TABLE doc_versions ADD COLUMN agent TEXT;
";

/// Agents' proposals to docs and tasks. Only an open one holds its `change`, whose bytes count
/// toward the doc quota; `change_hash` stays, so that one sent again answers as it first did.
/// The proposing machine is plain text, as it outlives the machine's pairing.
pub(crate) const PROPOSALS: &str = "
CREATE TABLE proposals (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    proposal_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    doc_id TEXT,
    task_id TEXT,
    new_id TEXT,
    base INTEGER,
    change TEXT,
    change_hash BLOB NOT NULL,
    snapshot TEXT,
    bytes INTEGER NOT NULL,
    author_member TEXT NOT NULL,
    agent TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    state TEXT NOT NULL,
    decided_by TEXT,
    version INTEGER,
    at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, proposal_id),
    FOREIGN KEY (workspace_id, doc_id) REFERENCES docs (workspace_id, doc_id),
    FOREIGN KEY (workspace_id, task_id) REFERENCES tasks (workspace_id, task_id),
    FOREIGN KEY (workspace_id, author_member) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, decided_by) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX proposals_open ON proposals (workspace_id, author_member) WHERE state = 'open';

CREATE TABLE changes_with_proposals (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    task_id TEXT,
    task_version INTEGER,
    doc_id TEXT,
    doc_version INTEGER,
    proposal_id TEXT,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, task_id, task_version)
        REFERENCES task_versions (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, doc_id, doc_version)
        REFERENCES doc_versions (workspace_id, doc_id, version),
    FOREIGN KEY (workspace_id, proposal_id) REFERENCES proposals (workspace_id, proposal_id)
) STRICT;

INSERT INTO changes_with_proposals
    (workspace_id, seq, kind, member_id, task_id, task_version, doc_id, doc_version)
SELECT workspace_id, seq, kind, member_id, task_id, task_version, doc_id, doc_version
FROM changes;

DROP TABLE changes;
ALTER TABLE changes_with_proposals RENAME TO changes;
";
