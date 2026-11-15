/// A membership is joined once it has been a member's, past ones too; one never joined is only
/// an invitation's. A membership's latest invitation keeps who made it and when it ends.
pub(crate) const INVITATIONS: &str = "
ALTER TABLE members ADD COLUMN joined INTEGER NOT NULL DEFAULT 1;

CREATE TABLE invitations (
    workspace_id TEXT NOT NULL,
    member_id TEXT NOT NULL,
    invited_by TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, member_id),
    FOREIGN KEY (workspace_id, member_id) REFERENCES members (workspace_id, member_id),
    FOREIGN KEY (workspace_id, invited_by) REFERENCES members (workspace_id, member_id)
) STRICT;
";
