//! A machine that connects again picks up its chats where it left them.

use tokio::sync::broadcast::error::RecvError;
use weacon_observability::{Fault, Field};
use weacon_protocol::Message;

use super::link;
use crate::http::Shared;
use crate::links::Connected;

/// How long after it was first sent a command is sent again to its machine when it connects;
/// older ones are left to their receipts, so nothing runs long after it was asked for.
const RESEND_WITHIN: i64 = 10 * 60;

/// Picks up each machine's chats as it connects, for as long as the server runs.
pub(crate) async fn follow_machines(shared: Shared) {
    let mut connected = shared.links.connected();
    loop {
        match connected.recv().await {
            Ok(machine) => {
                if let Err(fault) = pick_up(&shared, &machine).await {
                    let fields = [
                        ("machine_id", machine.machine.as_str().into()),
                        ("err", Field::error(&fault)),
                    ];
                    shared.log.warn("chats not picked up", &fields);
                }
            }
            // A connection missed here picks up its chats the next time it connects.
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return,
        }
    }
}

/// Follows again the chats on `connected`'s machine whose turn may not be over, in each of its
/// owner's workspaces, and sends again the commands to them sent within [`RESEND_WITHIN`]; the
/// core does each command once, whatever number of times it is sent.
async fn pick_up(shared: &Shared, connected: &Connected) -> Result<(), Fault> {
    let Connected { machine, owner } = connected;
    let workspaces = shared.db.account(owner.clone()).workspaces().await?;
    for summary in workspaces {
        let workspace = shared.db.workspace(&summary.id).await?;
        for (chat, core_seq) in workspace.unsettled_on(machine).await? {
            link::follow(shared, machine, workspace.id(), &chat, core_seq)?;
            let since = shared.now().saturating_sub(RESEND_WITHIN);
            let open = workspace.open_requests(&chat).await?.into_iter();
            let recent = open.filter(|(_, sent_at)| *sent_at >= since);
            for (command, _) in recent {
                link::send(shared, machine, Message::ChatCommand(command))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatQuery, ChatQueryKind};

    use super::*;
    use crate::chats::test_support::{ada, attach, sent};
    use crate::db::test_support::{NOW, id, make};
    use crate::test_support::Fixture;

    #[tokio::test]
    async fn a_machine_connecting_again_follows_its_unsettled_chats_and_gets_its_recent_commands() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        make(workspace, "c-2", "m1", NOW - RESEND_WITHIN - 1)
            .await
            .unwrap();
        make(workspace, "c-3", "m1", NOW).await.unwrap();
        workspace.close("c-3", "r-c-3").await;
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        let connected = Connected {
            machine: ada.machine.clone(),
            owner: ada.asker.account.clone(),
        };

        pick_up(shared, &connected).await.unwrap();

        let follow = |chat: &str| {
            Message::ChatQuery(ChatQuery {
                workspace: id("w1"),
                chat: id(chat),
                query: ChatQueryKind::Subscribe { after: 0 },
            })
        };
        let resent = |chat: &str| {
            let command =
                crate::db::test_support::command(&format!("r-{chat}"), chat, "Fix the build");
            Message::ChatCommand(command)
        };
        assert_eq!(
            sent(&mut attached),
            [follow("c-1"), resent("c-1"), follow("c-2")]
        );
        let elsewhere = Connected {
            machine: id("mc-other"),
            owner: ada.asker.account.clone(),
        };
        pick_up(shared, &elsewhere).await.unwrap();
        assert!(sent(&mut attached).is_empty());
    }
}
