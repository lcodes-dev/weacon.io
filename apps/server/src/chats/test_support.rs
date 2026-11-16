//! Ada and Grace, members of w1 each with a paired machine, and those machines' connections,
//! for chats' tests.

use weacon_protocol::{Asker, Envelope, MachineId, Message, WorkspaceId};

use crate::auth::Member;
use crate::db::test_support::id;
use crate::links::{Attached, Connected};
use crate::shared::Shared;
use crate::test_support::{Fixture, Running, email, member_id, pair_machine, run};

pub(crate) struct Ada {
    _server: Running,
    pub(crate) member: Member,
    pub(crate) asker: Asker,
    pub(crate) machine: MachineId,
}

// Ada, a member of w1, and the machine she paired.
pub(crate) async fn ada(fixture: &Fixture) -> Ada {
    let server = run(fixture.shared.clone()).await;
    let paired = pair_machine(fixture, server.address).await;
    let db = &fixture.shared.db;
    let account = db
        .account_with(&email("ada@example.com"))
        .await
        .unwrap()
        .unwrap();
    let workspace = db.workspace(&id::<WorkspaceId>("w1")).await.unwrap();
    let member = Member {
        workspace,
        id: member_id("m1"),
        token: String::new(),
    };
    Ada {
        _server: server,
        member,
        asker: Asker {
            account,
            member: member_id("m1"),
        },
        machine: id(paired.machine["id"].as_str().unwrap()),
    }
}

// Grace, another member of w1, and a machine of her own.
pub(crate) async fn grace(fixture: &Fixture) -> Ada {
    let db = &fixture.shared.db;
    let workspace = db.workspace(&id::<WorkspaceId>("w1")).await.unwrap();
    let account = crate::test_support::fresh_account();
    let address = email("grace@example.com");
    let m5 = member_id("m5");
    workspace.add_member(&m5, &account, &address).await.unwrap();
    workspace.pair_for("m5", "mc-grace").await;
    Ada {
        _server: run(fixture.shared.clone()).await,
        member: Member {
            workspace,
            id: member_id("m5"),
            token: String::new(),
        },
        asker: Asker {
            account,
            member: member_id("m5"),
        },
        machine: id("mc-grace"),
    }
}

/// `heard` with the first leg's run `run-1` under the name its own machine gives it, for
/// comparing with what that machine sent.
pub(crate) fn core_named<T: serde::Serialize + serde::de::DeserializeOwned>(heard: &T) -> T {
    let json = serde_json::to_string(heard).unwrap();
    serde_json::from_str(&json.replace("run-1-l1", "run-1")).unwrap()
}

pub(crate) fn attach(shared: &Shared, ada: &Ada, offers: &[&str]) -> Attached {
    let connected = Connected {
        machine: ada.machine.clone(),
        owner: ada.asker.account.clone(),
    };
    let offers = offers
        .iter()
        .map(|offer| serde_json::from_value(serde_json::json!(offer)).unwrap());
    shared.links.open(connected, offers.collect())
}

pub(crate) fn sent(attached: &mut Attached) -> Vec<Message> {
    std::iter::from_fn(|| attached.incoming.try_recv().ok())
        .map(|Envelope { message, .. }| message)
        .collect()
}
