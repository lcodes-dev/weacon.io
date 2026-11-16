/// Chats, the events their machines report, the prompts that wait for their owners and the
/// commands their owners sent them, and the feed's changes to them. Nothing reads `chat_bytes`.
pub(crate) const CHATS: &str = "
ALTER TABLE workspaces ADD COLUMN chat_bytes INTEGER NOT NULL DEFAULT 0;

CREATE TABLE chats (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    chat_id TEXT NOT NULL,
    owner_member TEXT NOT NULL,
    title TEXT NOT NULL,
    repo TEXT,
    machine_id TEXT NOT NULL,
    machine_name TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    seq INTEGER NOT NULL DEFAULT 0,
    core_seq INTEGER NOT NULL DEFAULT 0,
    run_id TEXT,
    run_status TEXT,
    PRIMARY KEY (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, owner_member) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX chats_updated ON chats (workspace_id, updated_at);
CREATE INDEX chats_machine ON chats (workspace_id, machine_id);

CREATE TABLE chat_events (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    core_seq INTEGER,
    body TEXT NOT NULL,
    at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, chat_id, seq),
    UNIQUE (workspace_id, chat_id, core_seq),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id)
) STRICT;

CREATE TABLE chat_waiting (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    prompt INTEGER NOT NULL,
    seq INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, chat_id, run_id, prompt),
    FOREIGN KEY (workspace_id, chat_id, seq) REFERENCES chat_events (workspace_id, chat_id, seq)
) STRICT;

CREATE TABLE chat_requests (
    workspace_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    command TEXT NOT NULL,
    sent_at INTEGER NOT NULL,
    open INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (workspace_id, request_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX chat_requests_open ON chat_requests (workspace_id, chat_id) WHERE open = 1;

CREATE TABLE changes_with_chats (
    workspace_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    kind TEXT NOT NULL,
    member_id TEXT NOT NULL,
    task_id TEXT,
    task_version INTEGER,
    doc_id TEXT,
    doc_version INTEGER,
    proposal_id TEXT,
    chat_id TEXT,
    PRIMARY KEY (workspace_id, seq),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, task_id, task_version)
        REFERENCES task_versions (workspace_id, task_id, version),
    FOREIGN KEY (workspace_id, doc_id, doc_version)
        REFERENCES doc_versions (workspace_id, doc_id, version),
    FOREIGN KEY (workspace_id, proposal_id) REFERENCES proposals (workspace_id, proposal_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id)
) STRICT;

INSERT INTO changes_with_chats
    (workspace_id, seq, kind, member_id, task_id, task_version, doc_id, doc_version, proposal_id)
SELECT workspace_id, seq, kind, member_id, task_id, task_version, doc_id, doc_version, proposal_id
FROM changes;

DROP TABLE changes;
ALTER TABLE changes_with_chats RENAME TO changes;
";

/// The task each chat was made for, if any, of its own workspace, with its title as it then was.
pub(crate) const CHAT_TASKS: &str = "
CREATE TABLE chat_tasks (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    task_id TEXT NOT NULL,
    title TEXT NOT NULL,
    PRIMARY KEY (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, task_id) REFERENCES tasks (workspace_id, task_id)
) STRICT;
";

/// Drops the commands kept to be sent again, as none is.
pub(crate) const REQUESTS_GONE: &str = "
DROP TABLE chat_requests;
";

/// Each member's own agent in a chat, in place of a chat's one machine, with the last of the
/// chat it was given as quotes; whose machine sent each event; and who last called their agent
/// in a chat, and when, until that agent's run reports. An event's place in its machine's log is kept by the leg alone from here on, as
/// two machines' places collide.
pub(crate) const CHAT_LEGS: &str = "
CREATE TABLE chat_legs (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    machine_name TEXT NOT NULL,
    agent TEXT NOT NULL,
    repo TEXT,
    core_seq INTEGER NOT NULL DEFAULT 0,
    detached INTEGER NOT NULL DEFAULT 0,
    quoted_seq INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (workspace_id, chat_id, member_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

CREATE INDEX chat_legs_machine ON chat_legs (workspace_id, machine_id);

INSERT INTO chat_legs (workspace_id, chat_id, member_id, machine_id, machine_name, agent, repo,
    core_seq, detached, quoted_seq)
SELECT c.workspace_id, c.chat_id, c.owner_member, c.machine_id, c.machine_name,
    COALESCE((SELECT json_extract(e.body, '$.agent') FROM chat_events AS e
        WHERE e.workspace_id = c.workspace_id AND e.chat_id = c.chat_id AND json_valid(e.body)
            AND json_extract(e.body, '$.kind') = 'prompt'
        ORDER BY e.seq LIMIT 1), 'claude'),
    c.repo, c.core_seq, c.state = 'detached', c.seq
FROM chats AS c;

UPDATE chats SET state = 'open' WHERE state = 'detached';

DROP INDEX chats_machine;
ALTER TABLE chats DROP COLUMN machine_id;
ALTER TABLE chats DROP COLUMN machine_name;
ALTER TABLE chats DROP COLUMN repo;
ALTER TABLE chats DROP COLUMN core_seq;
ALTER TABLE chats ADD COLUMN called_by TEXT;
ALTER TABLE chats ADD COLUMN called_at INTEGER;

ALTER TABLE chat_events ADD COLUMN member_id TEXT;

UPDATE chat_events SET member_id = (SELECT c.owner_member FROM chats AS c
    WHERE c.workspace_id = chat_events.workspace_id AND c.chat_id = chat_events.chat_id)
WHERE core_seq IS NOT NULL;

UPDATE chat_events SET core_seq = NULL;
";

/// Keys a chat's legs by their machine, which runs one agent in the chat whoever calls it, each
/// with the number it had, and notes which machine sent each event, which a call is for, and who
/// made each prompt the server took.
pub(crate) const LEGS_BY_MACHINE: &str = "
CREATE TABLE legs_by_machine (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    machine_name TEXT NOT NULL,
    agent TEXT NOT NULL,
    repo TEXT,
    core_seq INTEGER NOT NULL DEFAULT 0,
    detached INTEGER NOT NULL DEFAULT 0,
    quoted_seq INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (workspace_id, chat_id, machine_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

INSERT INTO legs_by_machine (rowid, workspace_id, chat_id, machine_id, member_id,
    machine_name, agent, repo, core_seq, detached, quoted_seq)
SELECT rowid, workspace_id, chat_id, machine_id, member_id, machine_name, agent, repo, core_seq,
    detached, quoted_seq
FROM chat_legs ORDER BY rowid;

ALTER TABLE chats ADD COLUMN called_on TEXT;

CREATE TABLE chat_calls (
    workspace_id TEXT NOT NULL,
    chat_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    machine_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY (workspace_id, chat_id, request_id),
    FOREIGN KEY (workspace_id, chat_id) REFERENCES chats (workspace_id, chat_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id)
) STRICT;

ALTER TABLE chat_events ADD COLUMN machine_id TEXT;

UPDATE chat_events SET machine_id = (SELECT l.machine_id FROM chat_legs AS l
    WHERE l.workspace_id = chat_events.workspace_id AND l.chat_id = chat_events.chat_id
        AND l.member_id = chat_events.member_id)
WHERE member_id IS NOT NULL;

DROP TABLE chat_legs;
ALTER TABLE legs_by_machine RENAME TO chat_legs;

CREATE INDEX chat_legs_machine ON chat_legs (workspace_id, machine_id);
";
