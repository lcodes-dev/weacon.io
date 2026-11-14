/// Chats, the events their machines report, the prompts that wait for their owners and the
/// commands their owners sent them, and the feed's changes to them. A workspace counts the
/// bytes its chats' events take.
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
