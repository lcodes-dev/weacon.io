/// The live and ended invitations of each workspace, one per address.
pub(crate) const INVITATIONS: &str = "
CREATE TABLE invitations (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    email TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    PRIMARY KEY (workspace_id, email)
) STRICT;
";

/// The machines each workspace's members may call agents on besides their own, as each
/// machine's core says its owner shares it.
pub(crate) const SHARED_MACHINES: &str = "
CREATE TABLE shared_machines (
    workspace_id TEXT NOT NULL REFERENCES workspaces (workspace_id),
    machine_id TEXT NOT NULL,
    PRIMARY KEY (workspace_id, machine_id)
) STRICT;
";
