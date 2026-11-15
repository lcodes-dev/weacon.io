//! A machine that connects again picks up its chats where it left them.

use std::future::Future;

use tokio::sync::broadcast::{Receiver, error::RecvError};
use tokio::task::JoinSet;
use weacon_observability::{Fault, Field};
use weacon_protocol::{ChatCommand, ChatId, Message, Outcome, Receipt, code};

use super::answers;
use super::link::{self, Route};
use crate::db::{Guard, OpenRequest, Workspace};
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

// Each machine is picked up on its own, so one slow to take its chats holds up no other.
async fn follow(shared: Shared, mut connected: Receiver<Connected>) {
    let mut picking = JoinSet::new();
    loop {
        while picking.try_join_next().is_some() {}
        match connected.recv().await {
            Ok(machine) => drop(picking.spawn(pick_up(shared.clone(), machine))),
            // Connections were missed: every machine connected now picks up its chats.
            Err(RecvError::Lagged(_)) => {
                for machine in shared.links.open_now() {
                    drop(picking.spawn(pick_up(shared.clone(), machine)));
                }
            }
            Err(RecvError::Closed) => break,
        }
    }
    picking.join_all().await;
}

async fn pick_up(shared: Shared, connected: Connected) {
    let machine = ("machine_id", connected.machine.as_str().into());
    let Some(route) = Route::open_now(&shared, &connected.machine) else {
        // A core that serves no chats is sent none, and its first prompts wait.
        match unserved(&shared, &connected).await {
            Ok(0) => {}
            Ok(unsent) => {
                let fields = [machine, ("unsent", unsent.into())];
                shared.log.warn("chats not all picked up", &fields);
            }
            Err(fault) => {
                let fields = [machine, ("err", Field::error(&fault))];
                shared.log.warn("chats not picked up", &fields);
            }
        }
        return;
    };
    if let Err(fault) = resend(&shared, &connected, &route).await {
        let fields = [machine, ("err", Field::error(&fault))];
        shared.log.warn("chats not picked up", &fields);
        // Connecting again, it is picked up again.
        route.close(&shared);
    }
}

/// Follows again `connected`'s unsettled chats in its owner's workspaces over `route`, then
/// sends again their open commands, oldest first, and the first prompts it never got.
async fn resend(shared: &Shared, connected: &Connected, route: &Route) -> Result<(), Fault> {
    let Connected { machine, owner } = connected;
    for summary in shared.db.account(owner.clone()).workspaces().await? {
        let workspace = shared.db.workspace(&summary.id).await?;
        for (chat, core_seq) in workspace.unsettled_on(machine).await? {
            let open = workspace.open_requests(&chat).await?;
            route
                .deliver(shared, link::follow(workspace.id(), &chat, core_seq), 0)
                .await?;
            for request in open {
                again(shared, &workspace, route, &chat, request).await?;
            }
        }
        first_prompts(shared, &workspace, route).await?;
    }
    Ok(())
}

// How many chats `connected` would have been sent, had its core served chats.
async fn unserved(shared: &Shared, connected: &Connected) -> Result<usize, Fault> {
    let Connected { machine, owner } = connected;
    let mut unserved = 0_usize;
    for summary in shared.db.account(owner.clone()).workspaces().await? {
        let workspace = shared.db.workspace(&summary.id).await?;
        unserved += workspace.unsettled_on(machine).await?.len();
        unserved += workspace.unmade_on(machine).await?.len();
    }
    Ok(unserved)
}

// Sends `request` again over `route` while it is recent; one its core refused for now and too
// old to send again ends in a failed receipt, and any other is left to its receipts. One being
// sent meanwhile is left to that send, and one sent lately isn't ended.
async fn again(
    shared: &Shared,
    workspace: &Workspace,
    route: &Route,
    chat: &ChatId,
    request: OpenRequest,
) -> Result<(), Fault> {
    let now = shared.now();
    if recent(now, request.sent_at) {
        if in_flight(shared, now, request.attempt_at) {
            return Ok(());
        }
        return send_again(shared, workspace, route, request.command)
            .await
            .map(drop);
    }
    if !request.refused || recent(now, request.attempt_at) {
        return Ok(());
    }
    let receipt = Receipt {
        request: request.command.request,
        outcome: Outcome::Failed {
            code: code!("chat.unavailable"),
        },
    };
    let guard = Guard {
        attempt: request.attempt,
        refused_only: true,
    };
    answers::end(shared, workspace, chat, receipt, guard).await
}

/// Sends `route`'s machine the first prompts in `workspace` it never got, and takes back the
/// chats of those not sent within [`RESEND_WITHIN`].
async fn first_prompts(shared: &Shared, workspace: &Workspace, route: &Route) -> Result<(), Fault> {
    for (chat, owner) in workspace.unmade_on(route.machine()).await? {
        let Some(first) = workspace.open_requests(&chat).await?.into_iter().next() else {
            workspace.forget_chat(&chat, 0).await?;
            continue;
        };
        // Being sent meanwhile: that send makes its chat. One begun lately may still be going
        // out, however long ago it was first sent, so it is neither made nor taken back.
        let now = shared.now();
        if recent(now, first.sent_at) {
            if in_flight(shared, now, first.attempt_at) {
                continue;
            }
            send_again(shared, workspace, route, first.command).await?;
        } else if recent(now, first.attempt_at) {
            continue;
        } else if !first.sent {
            workspace.forget_chat(&chat, first.attempt).await?;
            continue;
        }
        // Made only once it was queued, so no later prompt goes before it; one queued long ago
        // may have run, so its chat is followed rather than taken back.
        if !workspace.chat_made(&chat, &owner, None).await? {
            continue;
        }
        shared.wake_feeds(workspace.id());
        route
            .deliver(shared, link::follow(workspace.id(), &chat, 0), 0)
            .await?;
    }
    Ok(())
}

// Sends `command` again over `route` as a new attempt of its request, unless no open request
// is there to send; whether it was queued.
async fn send_again(
    shared: &Shared,
    workspace: &Workspace,
    route: &Route,
    command: ChatCommand,
) -> Result<bool, Fault> {
    let request = command.request.clone();
    let Some(attempt) = workspace.begin_attempt(&request, shared.now()).await? else {
        return Ok(false);
    };
    // Still refused until it is queued, so no later prompt goes before it.
    let sent = route
        .deliver(shared, Message::ChatCommand(command), attempt.number)
        .await;
    if let Err(fault) = sent {
        workspace.abandon(&request, attempt).await?;
        return Err(fault);
    }
    workspace.queued(&request, attempt).await?;
    Ok(true)
}

// Whether a send attempt begun at `attempt_at` may still be waiting, at `now`, for room to go out.
fn in_flight(shared: &Shared, now: i64, attempt_at: i64) -> bool {
    let waits = i64::try_from(shared.sockets.room_within.as_secs()).unwrap_or(i64::MAX);
    attempt_at >= now.saturating_sub(waits.saturating_add(1))
}

// Whether `at` was within [`RESEND_WITHIN`] of `now`.
fn recent(now: i64, at: i64) -> bool {
    at >= now.saturating_sub(RESEND_WITHIN)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, UNIX_EPOCH};

    use weacon_protocol::{ChatQuery, ChatQueryKind, RequestId, WorkspaceId};

    use super::*;
    use crate::chats::test_support::{Ada, ada, attach, sent};
    use crate::db::test_support::{Making, NOW, begin, id, make, make_as};
    use crate::test_support::Fixture;

    // `attached` once it has taken `count` messages, each within the tests' patience.
    async fn taken(mut attached: crate::links::Attached, count: usize) -> crate::links::Attached {
        for _ in 0..count {
            let next =
                tokio::time::timeout(crate::test_support::PATIENCE, attached.incoming.recv());
            next.await.unwrap().unwrap();
        }
        attached
    }

    // What `connected` is resent over the connection it has open now.
    async fn resend_now(shared: &Shared, connected: &Connected) -> Result<usize, Fault> {
        let Some(route) = Route::open_now(shared, &connected.machine) else {
            return unserved(shared, connected).await;
        };
        resend(shared, connected, &route).await.map(|()| 0)
    }

    // The outcome of `entry`, when it is a receipt.
    fn outcome(entry: weacon_protocol::ChatEntry) -> Option<Outcome> {
        let weacon_protocol::ChatEvent::Receipt { receipt } = entry.event else {
            return None;
        };
        Some(receipt.outcome)
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

        assert_eq!(resend_now(shared, &connected).await.unwrap(), 0);

        assert_eq!(
            sent(&mut attached),
            [follow_query("c-1"), resent("c-1"), follow_query("c-2")]
        );
        let elsewhere = Connected {
            machine: id("mc-other"),
            owner: ada.asker.account.clone(),
        };
        assert_eq!(resend_now(shared, &elsewhere).await.unwrap(), 0);
        assert!(sent(&mut attached).is_empty());
        pick_up(shared.clone(), connected.clone()).await;
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

        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);

        let first = crate::db::test_support::command("r-c-1", "c-1", "Fix the build");
        assert_eq!(
            sent(&mut attached),
            [Message::ChatCommand(first), follow_query("c-1")]
        );
        assert!(workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
        assert_eq!(workspace.chat(&id("c-2")).await.unwrap(), None);
        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);
        assert_eq!(sent(&mut attached), [follow_query("c-1"), resent("c-1")]);
    }

    #[tokio::test]
    async fn a_first_prompt_sent_long_ago_has_its_chat_made_and_followed_not_sent_again() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        begin(workspace, "c-1", "m1", NOW - 601).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        // Sent before the server stopped, its chat not yet made.
        workspace.mark_sent(&id("r-c-1"), true).await.unwrap();
        let mut attached = attach(shared, &ada, &["chat.serve"]);

        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);

        assert_eq!(sent(&mut attached), [follow_query("c-1")]);
        assert!(workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
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

        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 1);

        assert!(!workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
        drop(older);
        let mut serving = attach(shared, &ada, &["chat.serve"]);
        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);
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
        link::send_all(shared, &ada.machine, queued, 0).unwrap();

        pick_up(shared.clone(), connected(&ada)).await;

        assert!(!shared.links.is_open(&ada.machine));
        assert_eq!(sent(&mut full).len(), crate::links::QUEUE);
        // Not made, so no later prompt goes before it.
        assert!(!workspace.chat(&id("c-1")).await.unwrap().unwrap().made);
        let mut again = attach(shared, &ada, &["chat.serve"]);
        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);
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
        let attached = attach(shared, &ada, &["chat.serve"]);
        // Its follow, its first prompt and every command after it.
        let taking = tokio::spawn(taken(attached, crate::links::QUEUE + 2));

        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);

        let mut attached = taking.await.unwrap();
        assert!(sent(&mut attached).is_empty());
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

        assert_eq!(resend_now(&shared, &connected(&ada)).await.unwrap(), 0);

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

        pick_up(shared.clone(), connected(&ada)).await;

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
        assert_eq!(resend_now(shared, &connected).await.unwrap(), 2);
        pick_up(shared.clone(), connected.clone()).await;
        let warned = crate::test_support::messages(&fixture.records);
        assert!(warned.contains(&"chats not all picked up"), "{warned:?}");
        drop(older);
    }

    #[tokio::test]
    async fn a_machine_slow_to_take_its_chats_holds_up_no_other() {
        let fixture = Fixture::new().await;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.pair_for("m1", "mc-2").await;
        let elsewhere = Making {
            chat: "c-2",
            owner: "m1",
            title: "Fix the build",
            machine: "mc-2",
            at: NOW,
        };
        make_as(workspace, &elsewhere).await.unwrap();
        // Ada's first machine has no room for as long as the test runs.
        let mut shared = fixture.shared.clone();
        shared.sockets.room_within = Duration::from_hours(1);
        let _full = attach(&shared, &ada, &["chat.serve"]);
        let queued = (0..crate::links::QUEUE).map(|_| link::follow(&id("w1"), &id("c-9"), 0));
        link::send_all(&shared, &ada.machine, queued.collect(), 0).unwrap();
        let other = Connected {
            machine: id("mc-2"),
            owner: ada.asker.account.clone(),
        };
        let serve = weacon_protocol::Capability::new(link::SERVE);
        let mut quick = shared.links.open(other.clone(), vec![serve]);
        let (notices, connections) = tokio::sync::broadcast::channel(4);
        tokio::spawn(follow(shared.clone(), connections));

        notices.send(connected(&ada)).unwrap();
        notices.send(other).unwrap();

        let first = tokio::time::timeout(crate::test_support::PATIENCE, quick.incoming.recv());
        assert_eq!(first.await.unwrap().unwrap().message, follow_query("c-2"));
    }

    #[tokio::test]
    async fn an_old_refused_command_sent_again_lately_is_left_to_that_send() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW - 601).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.mark_refused(&id("r-c-1"), 0).await.unwrap();
        // A member's retry began a minute ago, and is still on its way to the queue.
        workspace
            .begin_attempt(&id("r-c-1"), NOW - 60)
            .await
            .unwrap();
        let _attached = attach(shared, &ada, &["chat.serve"]);

        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);

        assert_eq!(workspace.open_requests(&id("c-1")).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_command_refused_for_now_and_too_old_to_send_again_ends_in_one_failed_receipt() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW - 601).await.unwrap();
        make(workspace, "c-2", "m1", NOW - 601).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.mark_refused(&id("r-c-1"), 0).await.unwrap();
        let mut feeds = shared.changes.subscribe();
        let mut attached = attach(shared, &ada, &["chat.serve"]);

        // Two pick-ups of the same machine at once, as when it connects again quickly.
        let machine = connected(&ada);
        let (one, two) = tokio::join!(resend_now(shared, &machine), resend_now(shared, &machine));

        assert_eq!((one.unwrap(), two.unwrap()), (0, 0));
        // Each follows both, unless the other ended the refused one's chat first.
        let follows = sent(&mut attached);
        let count = |chat| {
            follows
                .iter()
                .filter(|message| **message == follow_query(chat))
                .count()
        };
        assert!((1..=2).contains(&count("c-1")), "{follows:?}");
        assert_eq!((count("c-2"), follows.len()), (2, count("c-1") + 2));
        let events = workspace.events_after(&id("c-1"), 0, 100, 1 << 20).await;
        let receipts: Vec<_> = events.unwrap().into_iter().filter_map(outcome).collect();
        let failed = Outcome::Failed {
            code: code!("chat.unavailable"),
        };
        assert_eq!(receipts, [failed]);
        assert_eq!(feeds.try_recv().unwrap(), id::<WorkspaceId>("w1"));
        // The one its core may have is left to its receipts; the ended one isn't followed again.
        assert_eq!(workspace.open_requests(&id("c-2")).await.unwrap().len(), 1);
        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);
        assert_eq!(sent(&mut attached), [follow_query("c-2")]);
    }

    #[tokio::test]
    async fn a_refused_command_not_sent_again_stays_refused_and_one_sent_since_isnt_ended() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.mark_refused(&id("r-c-1"), 0).await.unwrap();
        let _attached = attach(shared, &ada, &["chat.serve"]);
        let route = Route::open_now(shared, &ada.machine).unwrap();
        route.close(shared);
        let chat = id::<ChatId>("c-1");

        let request = workspace.open_requests(&chat).await.unwrap().pop().unwrap();
        again(shared, workspace, &route, &chat, request)
            .await
            .unwrap_err();

        let request = workspace.open_requests(&chat).await.unwrap().pop().unwrap();
        assert!(request.refused && request.attempt == 1, "{request:?}");
        // Being sent again by a member since the pick-up read it: that send's answer counts.
        workspace.begin_attempt(&id("r-c-1"), NOW).await.unwrap();
        let receipt = Receipt {
            request: id("r-c-1"),
            outcome: Outcome::Failed {
                code: code!("chat.unavailable"),
            },
        };
        let guard = Guard {
            attempt: request.attempt,
            refused_only: true,
        };
        answers::end(shared, workspace, &chat, receipt, guard)
            .await
            .unwrap();
        assert_eq!(workspace.open_requests(&chat).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_pick_up_whose_send_fails_undoes_no_other_pick_ups_send() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let older = attach(shared, &ada, &["chat.serve"]);
        let gone = Route::open_now(shared, &ada.machine).unwrap();
        let mut newer = attach(shared, &ada, &["chat.serve"]);
        drop(older);
        let route = Route::open_now(shared, &ada.machine).unwrap();
        let chat = id::<ChatId>("c-1");
        let stale = workspace.open_requests(&chat).await.unwrap().pop().unwrap();
        let read = workspace.open_requests(&chat).await.unwrap().pop().unwrap();

        // One pick-up sends it; the other, which read it unsent, then fails to.
        again(shared, workspace, &route, &chat, read).await.unwrap();
        again(shared, workspace, &gone, &chat, stale)
            .await
            .unwrap_err();

        let request = workspace.open_requests(&chat).await.unwrap().pop().unwrap();
        assert!(request.sent, "{request:?}");
        assert_eq!(sent(&mut newer), [resent("c-1")]);
        // The send that went out is still the one its core's answers count for.
        assert!(workspace.mark_refused(&id("r-c-1"), 1).await.unwrap());
    }

    #[tokio::test]
    async fn a_first_prompt_being_sent_is_neither_made_nor_taken_back_by_another_pick_up() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        begin(workspace, "c-1", "m1", NOW - 601).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        // A pick-up began sending it a minute ago, and is still on its way to the queue.
        workspace
            .begin_attempt(&id("r-c-1"), NOW - 60)
            .await
            .unwrap();

        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);

        let chat = workspace.chat(&id("c-1")).await.unwrap().unwrap();
        assert!(!chat.made);
        assert!(sent(&mut attached).is_empty());
    }

    #[tokio::test]
    async fn a_refused_command_holds_back_later_prompts_until_it_is_queued_again() {
        let fixture = Fixture::new().await;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        workspace.mark_refused(&id("r-c-1"), 0).await.unwrap();
        let mut shared = fixture.shared.clone();
        shared.sockets.room_within = Duration::from_hours(1);
        let mut full = attach(&shared, &ada, &["chat.serve"]);
        let queued = (0..crate::links::QUEUE).map(|_| link::follow(&id("w1"), &id("c-9"), 0));
        link::send_all(&shared, &ada.machine, queued.collect(), 0).unwrap();
        let route = Route::open_now(&shared, &ada.machine).unwrap();
        let chat = id::<ChatId>("c-1");
        let request = workspace.open_requests(&chat).await.unwrap().pop().unwrap();
        let later = id::<RequestId>("r-2");

        let resending = again(&shared, workspace, &route, &chat, request);
        let waiting = async {
            // The resend waits for room, and a later prompt still waits behind it.
            assert!(workspace.refused_before(&chat, &later).await.unwrap());
            full.incoming.recv().await.unwrap();
        };
        let (resent, ()) = tokio::join!(resending, waiting);

        resent.unwrap();
        assert!(!workspace.refused_before(&chat, &later).await.unwrap());
    }

    #[tokio::test]
    async fn a_command_forgotten_while_it_was_picked_up_is_never_sent() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        let route = Route::open_now(shared, &ada.machine).unwrap();
        let chat = id::<ChatId>("c-1");
        let read = workspace.open_requests(&chat).await.unwrap().pop().unwrap();

        // Its first sender's send failed and forgot it after the pick-up read it.
        workspace.forget_request(&id("r-c-1"), 0).await.unwrap();
        again(shared, workspace, &route, &chat, read).await.unwrap();

        assert!(sent(&mut attached).is_empty());
        // Queued once, a failed send of it no longer forgets it.
        make(workspace, "c-2", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let other = workspace
            .open_requests(&id("c-2"))
            .await
            .unwrap()
            .pop()
            .unwrap();
        again(shared, workspace, &route, &id("c-2"), other)
            .await
            .unwrap();
        workspace.forget_request(&id("r-c-2"), 1).await.unwrap();
        assert!(workspace.has_request(&id("r-c-2")).await.unwrap());
        assert_eq!(sent(&mut attached), [resent("c-2")]);
    }

    #[tokio::test]
    async fn a_first_prompt_forgotten_while_it_was_picked_up_is_never_sent() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        begin(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let mut attached = attach(shared, &ada, &["chat.serve"]);
        let route = Route::open_now(shared, &ada.machine).unwrap();
        let first = workspace
            .open_requests(&id("c-1"))
            .await
            .unwrap()
            .pop()
            .unwrap();

        workspace.forget_chat(&id("c-1"), 0).await.unwrap();
        let sent_again = send_again(shared, workspace, &route, first.command).await;
        assert!(!sent_again.unwrap());

        assert!(sent(&mut attached).is_empty());
    }

    #[tokio::test]
    async fn a_command_sent_again_counts_only_its_cores_next_answer() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        begin(workspace, "c-2", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        for request in ["r-c-1", "r-c-2"] {
            workspace.mark_refused(&id(request), 0).await.unwrap();
        }
        let _attached = attach(shared, &ada, &["chat.serve"]);

        assert_eq!(resend_now(shared, &connected(&ada)).await.unwrap(), 0);

        for chat in ["c-1", "c-2"] {
            let open = workspace.open_requests(&id(chat)).await.unwrap();
            assert!(open.iter().all(|request| !request.refused), "{open:?}");
        }
    }

    #[tokio::test]
    async fn a_pick_up_that_fails_closes_only_the_connection_it_was_for() {
        let fixture = Fixture::new().await;
        let shared = &fixture.shared;
        let ada = ada(&fixture).await;
        let workspace = &ada.member.workspace;
        make(workspace, "c-1", "m1", NOW).await.unwrap();
        workspace.move_chats_to(&ada.machine).await;
        let older = attach(shared, &ada, &["chat.serve"]);
        let route = Route::open_now(shared, &ada.machine).unwrap();
        let mut newer = attach(shared, &ada, &["chat.serve"]);
        drop(older);

        route.close(shared);
        assert!(shared.links.is_open(&ada.machine));
        let delivered = route.deliver(shared, follow_query("c-1"), 0).await;
        assert_eq!(delivered.unwrap_err().code(), &code!("machine.offline"));
        assert!(sent(&mut newer).is_empty());
        Route::open_now(shared, &ada.machine).unwrap().close(shared);
        assert!(!shared.links.is_open(&ada.machine));
    }
}
