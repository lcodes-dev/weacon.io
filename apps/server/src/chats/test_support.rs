//! Ada, a member of w1 with a paired machine, and that machine's connection, for chats' tests.

use weacon_protocol::{Asker, Envelope, MachineId, Message, WorkspaceId};

use crate::auth::Member;
use crate::db::test_support::id;
use crate::http::Shared;
use crate::links::{Attached, Connected};
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
        token_hash: [0; 32],
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
