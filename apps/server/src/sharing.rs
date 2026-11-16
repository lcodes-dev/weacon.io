//! The machines their owners share with a workspace: each core says which workspaces its
//! machine is shared with, and every member of one may call agents on it.

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use weacon_observability::Field;
use weacon_protocol::{MachineShares, SharedMachineList};

use crate::auth::{Machine, Member};
use crate::http::failure;
use crate::shared::Shared;
use crate::socket::Closed;

/// Keeps what `machine`'s core says it is shared with, in place of what it said before. When
/// the database fails, the link closes, so the core, connecting again, says it once more.
pub(crate) async fn keep(
    shared: &Shared,
    machine: &Machine,
    told: MachineShares,
) -> Result<(), Closed> {
    let kept = shared.db.set_shares(&machine.id, &told.workspaces).await;
    let machine_id = ("machine_id", machine.id.as_str().into());
    let fault = match kept {
        Ok(changed) => {
            shared.log.info("machine shares kept", &[machine_id]);
            // A chat whose member's agent was just detached shows so at once.
            for workspace in &changed {
                shared.wake_feeds(workspace);
            }
            return Ok(());
        }
        Err(fault) => fault,
    };
    let fields = [machine_id, ("err", Field::error(&fault))];
    shared.log.warn("machine shares not kept", &fields);
    Err(Closed)
}

/// `GET /api/workspaces/{workspace}/machines`: the machines shared with the workspace, to one
/// of its members.
pub(crate) async fn machines(State(shared): State<Shared>, member: Member) -> Response {
    match member.workspace.shared_machines().await {
        Ok(mut machines) => {
            for machine in &mut machines {
                machine.connected = shared.links.is_open(&machine.id);
            }
            Json(SharedMachineList { machines }).into_response()
        }
        Err(fault) => failure(fault),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::{Envelope, Message};

    use super::*;
    use crate::db::test_support::id;
    use crate::test_support::socket::{next, open, pongs, say, send};
    use crate::test_support::{Fixture, ask, email, fresh_account, member_id, pair_machine};

    #[tokio::test]
    async fn a_core_says_what_its_machine_is_shared_with_and_that_workspaces_members_see_it() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let paired = pair_machine(&fixture, server.address).await;
        let db = &fixture.shared.db;
        let (account, grace) = (fresh_account(), email("grace@example.com"));
        db.add_account(&account, &grace).await.unwrap();
        let w1 = db.workspace(&id("w1")).await.unwrap();
        w1.add_member(&member_id("m5"), &account, &grace)
            .await
            .unwrap();
        let grace = fixture.sign_in_client(server.address, "grace@example.com");
        let grace = grace.await;
        let mut core = open(server.address, "/api/machines/connect", &paired.token)
            .await
            .unwrap();
        say(&mut core, &["machine.connect"]).await;
        assert!(matches!(next(&mut core).await, Message::Welcome(_)));
        let listed = || async {
            let path = "GET /api/workspaces/w1/machines";
            ask(server.address, path, &grace.proof(), "").await.1
        };
        let says = |workspaces: &[&str]| Envelope {
            id: 0,
            message: Message::MachineShares(MachineShares {
                workspaces: workspaces.iter().copied().map(id).collect(),
            }),
        };

        let before = listed().await;
        send(&mut core, says(&["w1"])).await;
        // Its pong comes once the server has taken what was sent before it.
        assert!(pongs(&mut core).await);
        let shared = listed().await;
        send(&mut core, says(&[])).await;
        assert!(pongs(&mut core).await);
        let after = listed().await;

        assert_eq!(before, json!({ "machines": [] }));
        let machine = json!({ "id": paired.machine["id"], "name": paired.machine["name"],
                              "owner": "m1", "connected": true });
        assert_eq!(shared, json!({ "machines": [machine] }));
        assert_eq!(after, before);
        // A share the database can't keep ends the link, so the core says it again.
        w1.break_shares().await;
        send(&mut core, says(&["w1"])).await;
        assert!(crate::test_support::socket::ends(&mut core).await);
    }
}
