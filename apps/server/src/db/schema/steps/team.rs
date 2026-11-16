/// The live and ended invitations of each workspace, one per address.
pub(crate) const INVITATIONS: &str = "
CREATE TABLE invitations (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    email TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, email)
) STRICT;
";
