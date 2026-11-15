//! A machine that connects again picks up its chats where it left them.

use std::future::Future;

use tokio::sync::broadcast::{Receiver, error::RecvError};
use weacon_observability::{Fault, Field};
use weacon_protocol::{MachineId, Message};

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
/// commands sent within [`RESEND_WITHIN`], which the core does once; gives how many weren't sent.
async fn resend(shared: &Shared, connected: &Connected) -> Result<usize, Fault> {
    let Connected { machine, owner } = connected;
    let mut unsent = 0_usize;
    for summary in shared.db.account(owner.clone()).workspaces().await? {
        let workspace = shared.db.workspace(&summary.id).await?;
        for (chat, core_seq) in workspace.unsettled_on(machine).await? {
            let open = workspace.open_requests(&chat).await?.into_iter();
            let commands = open.filter(|(_, sent_at)| recent(shared, *sent_at));
            let follow = link::follow(workspace.id(), &chat, core_seq);
            let sent: Vec<_> = std::iter::once(follow)
                .chain(commands.map(|(command, _)| Message::ChatCommand(command)))
                .collect();
            // What can't be sent now is sent the next time the machine connects.
            let count = sent.len();
            if link::send_all(shared, machine, sent).is_err() {
                unsent += count;
            }
        }
        unsent += first_prompts(shared, &workspace, machine).await?;
    }
    Ok(unsent)
}

/// Sends `machine` the first prompts in `workspace` it never got, as when the server stopped
/// before sending one, and takes back the chats of those not sent within [`RESEND_WITHIN`];
/// gives how many weren't sent. Each chat is made before its prompt is sent, so a first prompt
/// failing elsewhere meanwhile can't take it back, and one not sent stays to be sent again.
async fn first_prompts(
    shared: &Shared,
    workspace: &Workspace,
    machine: &MachineId,
) -> Result<usize, Fault> {
    let mut unsent = 0_usize;
    for (chat, owner) in workspace.unmade_on(machine).await? {
        let first = workspace.open_requests(&chat).await?.into_iter().next();
        let Some((command, _)) = first.filter(|(_, sent_at)| recent(shared, *sent_at)) else {
            workspace.forget_chat(&chat).await?;
            continue;
        };
        if !workspace.chat_made(&chat, &owner).await? {
            continue;
        }
        shared.wake_feeds(workspace.id());
        let sent = [
            Message::ChatCommand(command),
            link::follow(workspace.id(), &chat, 0),
        ];
        if link::send_all(shared, machine, sent.into()).is_err() {
            unsent += 2;
        }
    }
    Ok(unsent)
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
    async fn a_first_prompt_the_machine_cant_take_is_counted_and_sent_again_next_time() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        begin(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let older = attach(shared, &ada, &["machine.connect"]);

        // Its prompt and its follow.
        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 2);

        assert!(workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
        drop(older);
        let mut serving = attach(shared, &ada, &["chat.serve"]);
        assert_eq!(resend(shared, &connected(&ada)).await.unwrap(), 0);
        assert_eq!(sent(&mut serving), [follow_query("c-1"), resent("c-1")]);
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
        // Each chat's follow and its one open command, none of which this core serves.
        assert_eq!(resend(shared, &connected).await.unwrap(), 4);
        pick_up(shared, &connected).await;
        let warned = crate::test_support::messages(&fixture.records);
        assert!(warned.contains(&"chats not all picked up"), "{warned:?}");
        drop(older);
    }
}
