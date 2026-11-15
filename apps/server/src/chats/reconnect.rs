//! A machine that connects again picks up its chats where it left them.

use std::future::Future;

use tokio::sync::broadcast::{Receiver, error::RecvError};
use weacon_observability::{Fault, Field};
use weacon_protocol::{Capability, MachineId, Message};

use super::link;
use crate::db::Workspace;
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
    match resend(shared, connected).await {
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

/// Follows again `connected`'s unsettled chats in its owner's workspaces and sends again their
/// commands sent within [`RESEND_WITHIN`], which the core does once, oldest first. A core that
/// serves no chats is sent none, and gives how many it would have followed; one that can't take
/// them all is `machine.offline`, and its link is closed so they are all sent again.
async fn resend(shared: &Shared, connected: &Connected) -> Result<usize, Fault> {
    let Connected { machine, owner } = connected;
    let serving = shared.links.sender(machine, &Capability::new(link::SERVE));
    let mut unserved = 0_usize;
    for summary in shared.db.account(owner.clone()).workspaces().await? {
        let workspace = shared.db.workspace(&summary.id).await?;
        let unsettled = workspace.unsettled_on(machine).await?;
        if serving.is_none() {
            unserved += unsettled.len() + workspace.unmade_on(machine).await?.len();
            continue;
        }
        for (chat, core_seq) in unsettled {
            let open = workspace.open_requests(&chat).await?.into_iter();
            link::deliver(
                shared,
                machine,
                link::follow(workspace.id(), &chat, core_seq),
            )
            .await?;
            for (command, _) in open.filter(|(_, sent_at)| recent(shared, *sent_at)) {
                link::deliver(shared, machine, Message::ChatCommand(command)).await?;
            }
        }
        first_prompts(shared, &workspace, machine).await?;
    }
    Ok(unserved)
}

/// Sends `machine` the first prompts in `workspace` it never got, as when the server stopped
/// before sending one, and takes back the chats of those not sent within [`RESEND_WITHIN`].
/// A first prompt is marked sent before it is, so its chat failing elsewhere meanwhile can't
/// be taken back, and its chat is made only once it was, so no later prompt goes before it.
async fn first_prompts(
    shared: &Shared,
    workspace: &Workspace,
    machine: &MachineId,
) -> Result<(), Fault> {
    for (chat, owner) in workspace.unmade_on(machine).await? {
        let first = workspace.open_requests(&chat).await?.into_iter().next();
        let Some((command, _)) = first.filter(|(_, sent_at)| recent(shared, *sent_at)) else {
            workspace.forget_chat(&chat).await?;
            continue;
        };
        let request = command.request.clone();
        workspace.mark_sent(&request, true).await?;
        if let Err(fault) = link::deliver(shared, machine, Message::ChatCommand(command)).await {
            workspace.mark_sent(&request, false).await?;
            return Err(fault);
        }
        if !workspace.chat_made(&chat, &owner).await? {
            continue;
        }
        shared.wake_feeds(workspace.id());
        link::deliver(shared, machine, link::follow(workspace.id(), &chat, 0)).await?;
    }
    Ok(())
}

// Whether a command sent at `sent_at` was sent within [`RESEND_WITHIN`] of now.
fn recent(shared: &Shared, sent_at: i64) -> bool {
    sent_at >= shared.now().saturating_sub(RESEND_WITHIN)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    use weacon_protocol::{ChatQuery, ChatQueryKind, WorkspaceId};

    use super::*;
    use crate::chats::test_support::{Ada, ada, attach, sent};
    use crate::db::test_support::{NOW, begin, id, make};
    use crate::test_support::Fixture;

    // How many messages `attached` takes until its connection ends.
    async fn taken_all(attached: &mut crate::links::Attached) -> usize {
        let mut taken = 0_usize;
        while attached.incoming.recv().await.is_some() {
            taken += 1;
        }
        taken
    }

    fn connected(ada: &Ada) -> Connected {
        Connected {
            machine: ada.machine.clone(),
            owner: ada.asker.account.clone(),
        }
    }

    fn follow_query(chat: &str) -> Message {
        Message::ChatQuery(ChatQuery {
            workspace: id("w1"),
            chat: id(chat),
            query: ChatQueryKind::Subscribe { after: 0 },
        })
    }

    fn resent(chat: &str) -> Message {
        let command = crate::db::test_support::command(&format!("r-{chat}"), chat, "Fix the build");
        Message::ChatCommand(command)
    }

    #[tokio::test]
    async fn a_machine_connecting_again_follows_its_unsettled_chats_and_gets_its_recent_commands() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        // Ten minutes is the window: sent 599 seconds ago is sent again, 601 is not.
        make(workspace, "c-1", "m1", NOW - 599).await.unwrap();
        make(workspace, "c-2", "m1", NOW - 601).await.unwrap();
        make(workspace, "c-3", "m1", NOW).await.unwrap();
        workspace.close_request("c-3", "r-c-3").await;
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        let connected = Connected {
            machine: ada.machine.clone(),
            owner: ada.asker.account.clone(),
        };

        assert_eq!(resend(shared, &connected).await.unwrap(), 0);

        assert_eq!(
            sent(&mut attached),
            [follow_query("c-1"), resent("c-1"), follow_query("c-2")]
        );
        let elsewhere = Connected {
            machine: id("mc-other"),
            owner: ada.asker.account.clone(),
        };
        assert_eq!(resend(shared, &elsewhere).await.unwrap(), 0);
        assert!(sent(&mut attached).is_empty());
        pick_up(shared, &connected).await;
        let warned = crate::test_support::messages(&fixture.records);
        assert!(!warned.contains(&"chats not all picked up"), "{warned:?}");
    }

    #[tokio::test]
    async fn a_first_prompt_the_machine_never_got_is_sent_and_one_too_old_is_taken_back() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        begin(workspace, "c-1", "m1", NOW - 599).await.unwrap();
        begin(workspace, "c-2", "m1", NOW - 601).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut feeds = shared.changes.subscribe();
        let mut attached = attach(shared, &ada, &["chat.serve"]);

        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 0);

        let first = crate::db::test_support::command("r-c-1", "c-1", "Fix the build");
        assert_eq!(
            sent(&mut attached),
            [Message::ChatCommand(first), follow_query("c-1")]
        );
        assert!(workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
        assert_eq!(workspace.chat(&id("c-2")).await.unwrap(), None);
        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 0);
        assert_eq!(sent(&mut attached), [follow_query("c-1"), resent("c-1")]);
    }

    #[tokio::test]
    async fn a_first_prompt_waits_for_a_core_that_serves_chats() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        begin(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let older = attach(shared, &ada, &["machine.connect"]);

        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 1);

        assert!(!workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
        drop(older);
        let mut serving = attach(shared, &ada, &["chat.serve"]);
        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 0);
        assert_eq!(sent(&mut serving), [resent("c-1"), follow_query("c-1")]);
        assert!(workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
    }

    #[tokio::test]
    async fn a_core_without_room_for_its_chats_connects_again_and_gets_them_first() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        begin(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut full = attach(shared, &ada, &["chat.serve"]);
        let other = || link::follow(&id("w1"), &id("c-9"), 0);
        let queued = (0..crate::links::QUEUE).map(|_| other()).collect();
        link::send_all(shared, &ada.machine, queued).unwrap();

        pick_up(shared, &connected(&ada)).await;

        assert!(!shared.links.is_open(&ada.machine));
        assert_eq!(sent(&mut full).len(), crate::links::QUEUE);
        // Not made, so no later prompt goes before it.
        assert!(!workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
        let mut again = attach(shared, &ada, &["chat.serve"]);
        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 0);
        assert_eq!(sent(&mut again), [resent("c-1"), follow_query("c-1")]);
        assert!(workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
    }

    #[tokio::test]
    async fn more_commands_than_the_queue_holds_reach_a_machine_that_takes_them() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let member = crate::test_support::member_id("m1");
        for n in 0..crate::links::QUEUE {
            let asked = crate::db::test_support::command(&format!("r-{n}"), "c-1", "Go on");
            let request = crate::db::NewRequest {
                command: &asked,
                member: &member,
                at: NOW,
            };
            workspace.record_request(&request).await.unwrap();
        }
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        let taken = tokio::spawn(async move { taken_all(&mut attached).await });

        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 0);

        shared.links.close(&ada.machine);
        // Its follow, its first prompt and every command after it.
        assert_eq!(taken.await.unwrap(), crate::links::QUEUE + 2);
    }

    #[tokio::test]
    async fn a_command_is_too_old_to_resend_by_the_time_it_would_be_sent() {
        let fixture = Fixture::new().await;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW - 599).await.unwrap();
        make(workspace, "c-2", "m1", NOW - 599).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        // Two seconds pass each time the clock is read.
        let mut shared = fixture.shared.clone();
        let reads = Arc::new(AtomicU64::new(0));
        let start = u64::try_from(NOW).unwrap();
        shared.clock = Arc::new(Box::new(move || {
            let read = reads.fetch_add(1, Ordering::Relaxed);
            UNIX_EPOCH + Duration::from_secs(start + 2 * read)
        }));
        let mut attached = attach(&shared, &ada, &["chat.serve"]);

        assert_eq!(resend(&shared, &connected(&ada)).await.unwrap(), 0);

        assert_eq!(
            sent(&mut attached),
            [follow_query("c-1"), resent("c-1"), follow_query("c-2")]
        );
    }

    #[tokio::test]
    async fn connections_missed_pick_up_every_machine_connected_now() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
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

        assert_eq!(sent(&mut attached), [follow_query("c-1"), resent("c-1")]);
    }

    #[tokio::test]
    async fn a_machine_whose_chats_cant_be_read_connects_again() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        make(&ada.member.workspace, "c-1", "m1", NOW).await.unwrap();
        ada.member.workspace.move_chats_to(&ada.machine).await;
        ada.member.workspace.break_chat_requests().await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);

        pick_up(shared, &connected(&ada)).await;

        assert!(!shared.links.is_open(&ada.machine));
        assert_eq!(
            attached.incoming.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected)
        );
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

        let first = tokio::time::timeout(crate::test_support::PATIENCE, attached.incoming.recv());
        let first = first.await.unwrap().unwrap();
        assert!(matches!(first.message, Message::ChatQuery(_)), "{first:?}");
        drop(attached);
        let older = attach(shared, &ada, &["machine.connect"]);
        let connected = Connected {
            machine: ada.machine.clone(),
            owner: ada.asker.account.clone(),
        };
        // Both chats, which this core doesn't serve.
        assert_eq!(resend(shared, &connected).await.unwrap(), 2);
        pick_up(shared, &connected).await;
        let warned = crate::test_support::messages(&fixture.records);
        assert!(warned.contains(&"chats not all picked up"), "{warned:?}");
        drop(older);
    }
}
