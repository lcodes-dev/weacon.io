//! A machine that connects again picks up its chats where it left them.

use std::future::Future;

use tokio::sync::broadcast::{Receiver, error::RecvError};
use weacon_observability::{Fault, Field};

use super::link;
use crate::links::Connected;
use crate::shared::Shared;

/// Picks up each machine's chats as it connects, from now on, for as long as the server runs.
pub(crate) fn follow_machines(shared: Shared) -> impl Future<Output = ()> {
    let connected = shared.links.connected();
    follow(shared, connected)
}

async fn follow(shared: Shared, mut connected: Receiver<Connected>) {
    loop {
        match connected.recv().await {
            Ok(machine) => pick_up(&shared, &machine).await,
            // Connections were missed: every machine connected now picks up its chats.
            Err(RecvError::Lagged(_)) => {
                for machine in shared.links.open_now() {
                    pick_up(&shared, &machine).await;
                }
            }
            Err(RecvError::Closed) => return,
        }
    }
}

async fn pick_up(shared: &Shared, connected: &Connected) {
    let machine = ("machine_id", connected.machine.as_str().into());
    match follow_again(shared, connected).await {
        Err(fault) => {
            let fields = [machine, ("err", Field::error(&fault))];
            shared.log.warn("chats not picked up", &fields);
            // Connecting again, it is picked up again.
            shared.links.close(&connected.machine);
        }
        Ok(0) => {}
        Ok(unsent) => {
            let fields = [machine, ("unsent", unsent.into())];
            shared.log.warn("chats not all picked up", &fields);
        }
    }
}

/// Follows again `connected`'s open chats in its owner's workspaces, so it sends up what it did
/// meanwhile; gives how many weren't followed.
async fn follow_again(shared: &Shared, connected: &Connected) -> Result<usize, Fault> {
    let Connected { machine, owner } = connected;
    let mut unsent = 0_usize;
    for summary in shared.db.account(owner.clone()).workspaces().await? {
        let workspace = shared.db.workspace(&summary.id).await?;
        for (chat, core_seq) in workspace.open_on(machine).await? {
            let follow = link::follow(workspace.id(), &chat, core_seq);
            // What can't be followed now is the next time the machine connects.
            let sent = vec![(shared.links.next_id(), follow)];
            if link::send_all(shared, machine, link::SERVE, sent).is_err() {
                unsent += 1;
            }
        }
    }
    Ok(unsent)
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatQuery, ChatQueryKind, Message, RunState};

    use super::*;
    use crate::chats::test_support::{Ada, ada, attach, sent};
    use crate::db::test_support::{NOW, append, id, make, status};
    use crate::test_support::Fixture;

    fn connected(ada: &Ada) -> Connected {
        Connected {
            machine: ada.machine.clone(),
            owner: ada.asker.account.clone(),
        }
    }

    fn follow_query(chat: &str, after: u64) -> Message {
        Message::ChatQuery(ChatQuery {
            workspace: id("w1"),
            chat: id(chat),
            query: ChatQueryKind::Subscribe { after },
        })
    }

    // Ada's chats c-1, whose machine sent up to its event 3, and c-2, on her machine.
    async fn chats(ada: &Ada) {
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        make(workspace, "c-2", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        append(workspace, "c-1", &[(Some(3), status(RunState::Running))])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_machine_connecting_again_follows_its_open_chats_and_nothing_else() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        chats(&ada).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);

        assert_eq!(follow_again(shared, &connected(&ada)).await.unwrap(), 0);

        assert_eq!(
            sent(&mut attached),
            [follow_query("c-1", 3), follow_query("c-2", 0)]
        );
        let elsewhere = Connected {
            machine: id("mc-other"),
            owner: ada.asker.account.clone(),
        };
        assert_eq!(follow_again(shared, &elsewhere).await.unwrap(), 0);
        assert!(sent(&mut attached).is_empty());
    }

    #[tokio::test]
    async fn connections_missed_pick_up_every_machine_connected_now() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        chats(&ada).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        let (notices, missed) = tokio::sync::broadcast::channel(1);
        let elsewhere = Connected {
            machine: id("mc-other"),
            owner: ada.asker.account.clone(),
        };
        notices.send(elsewhere.clone()).unwrap();
        notices.send(elsewhere).unwrap();
        drop(notices);

        follow(shared.clone(), missed).await;

        assert_eq!(
            sent(&mut attached),
            [follow_query("c-1", 3), follow_query("c-2", 0)]
        );
    }

    #[tokio::test]
    async fn each_machine_that_connects_is_followed_and_what_it_cant_take_is_counted() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        chats(&ada).await;
        tokio::spawn(follow_machines(shared.clone()));

        let mut attached = attach(shared, &ada, &["chat.serve"]);

        let first = tokio::time::timeout(crate::test_support::PATIENCE, attached.incoming.recv());
        let first = first.await.unwrap().unwrap();
        assert_eq!(first.message, follow_query("c-1", 3));
        drop(attached);
        let older = attach(shared, &ada, &["machine.connect"]);
        assert_eq!(follow_again(shared, &connected(&ada)).await.unwrap(), 2);
        pick_up(shared, &connected(&ada)).await;
        let warned = crate::test_support::messages(&fixture.records);
        assert!(warned.contains(&"chats not all picked up"), "{warned:?}");
        drop(older);
    }
}
