//! A machine that connects again picks up its chats where it left them.

use std::future::Future;

use tokio::sync::broadcast::{Receiver, error::RecvError};
use weacon_observability::{Fault, Field};
use weacon_protocol::Message;

use super::link;
use crate::links::Connected;
use crate::shared::Shared;

/// How long after it was first sent a command is sent again to its machine when it connects;
/// older ones are left to their receipts, so nothing runs long after it was asked for.
const RESEND_WITHIN: i64 = 10 * 60;

/// Picks up each machine's chats as it connects, from now on, for as long as the server runs.
pub(crate) fn follow_machines(shared: Shared) -> impl Future<Output = ()> {
    let connected = shared.links.connected();
    follow(shared, connected)
}

async fn follow(shared: Shared, mut connected: Receiver<Connected>) {
    loop {
        match connected.recv().await {
            Ok(machine) => pick_up(&shared, &machine).await,
            // A connection missed here picks up its chats the next time it connects.
            Err(RecvError::Lagged(_)) => {}
            Err(RecvError::Closed) => return,
        }
    }
}

async fn pick_up(shared: &Shared, connected: &Connected) {
    let (unsent, failed) = match resend(shared, connected).await {
        Ok(unsent) => (unsent, None),
        Err(fault) => (0, Some(fault)),
    };
    let machine = ("machine_id", connected.machine.as_str().into());
    if let Some(fault) = failed {
        let fields = [machine, ("err", Field::error(&fault))];
        shared.log.warn("chats not picked up", &fields);
    } else if unsent > 0 {
        shared.log.warn(
            "chats not all picked up",
            &[machine, ("unsent", unsent.into())],
        );
    }
}

/// Follows again `connected`'s unsettled chats in its owner's workspaces and sends again their
/// commands sent within [`RESEND_WITHIN`], which the core does once; gives how many weren't sent.
async fn resend(shared: &Shared, connected: &Connected) -> Result<usize, Fault> {
    let Connected { machine, owner } = connected;
    let since = shared.now().saturating_sub(RESEND_WITHIN);
    let mut unsent = 0_usize;
    for summary in shared.db.account(owner.clone()).workspaces().await? {
        let workspace = shared.db.workspace(&summary.id).await?;
        for (chat, core_seq) in workspace.unsettled_on(machine).await? {
            // One that can't be sent now is sent the next time the machine connects.
            let followed = link::follow(shared, machine, workspace.id(), &chat, core_seq);
            unsent += usize::from(followed.is_err());
            let open = workspace.open_requests(&chat).await?.into_iter();
            for (command, _) in open.filter(|(_, sent_at)| *sent_at >= since) {
                let sent = link::send(shared, machine, Message::ChatCommand(command));
                unsent += usize::from(sent.is_err());
            }
        }
    }
    Ok(unsent)
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
        workspace.close_request("c-3", "r-c-3").await;
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        let connected = Connected {
            machine: ada.machine.clone(),
            owner: ada.asker.account.clone(),
        };

        assert_eq!(resend(shared, &connected).await.unwrap(), 0);

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
        assert_eq!(resend(shared, &elsewhere).await.unwrap(), 0);
        assert!(sent(&mut attached).is_empty());
    }

    #[tokio::test]
    async fn each_machine_that_connects_is_followed_and_what_it_cant_take_is_counted_once() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        make(workspace, "c-2", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        tokio::spawn(follow_machines(shared.clone()));

        let mut attached = attach(shared, &ada, &["chat.serve"]);

        let first = attached.incoming.recv().await.unwrap();
        assert!(matches!(first.message, Message::ChatQuery(_)), "{first:?}");
        drop(attached);
        let older = attach(shared, &ada, &["machine.connect"]);
        let connected = Connected {
            machine: ada.machine.clone(),
            owner: ada.asker.account.clone(),
        };
        // Each chat's follow and its one open command, none of which this core serves.
        assert_eq!(resend(shared, &connected).await.unwrap(), 4);
        pick_up(shared, &connected).await;
        let warned = crate::test_support::messages(&fixture.records);
        assert!(warned.contains(&"chats not all picked up"), "{warned:?}");
        drop(older);
    }
}
