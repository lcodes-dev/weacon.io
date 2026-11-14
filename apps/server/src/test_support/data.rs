//! Made-up data tests store.

use weacon_testing::TempDir;

use crate::db::{Db, Workspace};
use crate::email::Email;

/// Workspaces w1 and w2 on a new database. ada@example.com is m1 in w1 and m2 in w2, and
/// bo@example.com is m3 in w1 only.
pub(crate) async fn two_teams(dir: &TempDir) -> (Workspace, Workspace) {
    let db = Db::create(&dir.join("weacon.db")).await.unwrap();
    let one = db
        .create_workspace(&workspace_id("w1"), "One")
        .await
        .unwrap();
    let two = db
        .create_workspace(&workspace_id("w2"), "Two")
        .await
        .unwrap();
    for (workspace, member, address) in [
        (&one, "m1", "ada@example.com"),
        (&two, "m2", "ada@example.com"),
        (&one, "m3", "bo@example.com"),
    ] {
        let email = email(address);
        workspace
            .add_member(&member_id(member), &fresh_account(), &email)
            .await
            .unwrap();
    }
    (one, two)
}

pub(crate) fn workspace_id(id: &str) -> weacon_protocol::WorkspaceId {
    weacon_protocol::WorkspaceId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn member_id(id: &str) -> weacon_protocol::MemberId {
    weacon_protocol::MemberId::try_from(id.to_owned()).unwrap()
}

/// An account id nothing else has.
pub(crate) fn fresh_account() -> weacon_protocol::AccountId {
    weacon_protocol::AccountId::try_from(crate::app::new_id()).unwrap()
}

pub(crate) fn email(address: &str) -> Email {
    Email::parse(address).unwrap()
}

/// A task to do, at its first version.
pub(crate) fn new_task(id: &str, title: &str, order: &str) -> weacon_model::Task {
    weacon_model::Task::new(
        weacon_protocol::TaskId::try_from(id.to_owned()).unwrap(),
        weacon_model::Title::try_from(title.to_owned()).unwrap(),
        weacon_model::OrderKey::try_from(order.to_owned()).unwrap(),
    )
}

/// An agent of `kind`, whose owner is `owner`, on the machine called `machine` when known.
pub(crate) fn agent(
    kind: weacon_protocol::AgentKind,
    owner: weacon_protocol::AgentOwner,
    machine: Option<&str>,
) -> weacon_protocol::AgentRef {
    weacon_protocol::AgentRef {
        kind,
        owner,
        machine: machine
            .map(|name| weacon_protocol::MachineName::try_from(name.to_owned()).unwrap()),
    }
}
