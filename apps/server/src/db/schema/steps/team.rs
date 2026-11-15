/// A membership is joined once it has been a member's, past ones too; one never joined is only
/// an invitation's. Each invitation a membership had keeps who made it, when it ends, whether
/// its address hid it and when its address was last told of it.
pub(crate) const INVITATIONS: &str = "
ALTER TABLE members ADD COLUMN joined INTEGER NOT NULL DEFAULT 1;

CREATE TABLE invitations (
    workspace_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    invited_by TEXT NOT NULL,
    invited_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    hidden INTEGER NOT NULL DEFAULT 0,
    noticed_at INTEGER,
    PRIMARY KEY (workspace_id, member_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, invited_by) REFERENCES members (workspace_id, member_id)
) STRICT;
";
