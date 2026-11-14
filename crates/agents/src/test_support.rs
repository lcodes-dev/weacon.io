//! Helpers for the drivers' tests: a run over a replayed session, and the events it yields.

use std::time::Duration;

use tokio::io::{BufReader, DuplexStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use weacon_protocol::{AgentSession, App, Outcome, PermissionAnswer, RunEvent, RunId};
use weacon_testing::{Failure, replay};

use crate::access::Rules;
use crate::run::Run;
use crate::wire::Wire;

pub(crate) type FakeRun = Run<DuplexStream, BufReader<DuplexStream>>;

pub(crate) type Done = JoinHandle<Result<(), Failure>>;

/// A run speaking `wire` to a fake agent replaying `recording`.
pub(crate) fn fake(wire: Wire, recording: &str) -> (FakeRun, Done) {
    let replay = replay(recording);
    let run = Run::new(
        run_id("run-1"),
        wire,
        replay.stdin,
        replay.stdout,
        Rules::ask_first(),
    );
    (run, replay.done)
}

pub(crate) fn app() -> App {
    App {
        name: "weacon-core".to_owned(),
        version: "0.1.0".to_owned(),
        commit: "1a2b3c4".to_owned(),
    }
}

// Under paused time, a driver and fake stuck waiting on each other time out at once.
pub(crate) async fn next(run: &mut FakeRun) -> Option<RunEvent> {
    timeout(Duration::from_secs(5), run.next())
        .await
        .unwrap()
        .unwrap()
}

/// Events up to and including the first that `last` picks, or until the agent exits.
pub(crate) async fn until(run: &mut FakeRun, last: fn(&RunEvent) -> bool) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) = next(run).await {
        let done = last(&event);
        events.push(event);
        if done {
            break;
        }
    }
    events
}

pub(crate) async fn ends_as_recorded(mut run: FakeRun, done: Done) {
    assert_eq!(next(&mut run).await, None);
    drop(run);
    timeout(Duration::from_secs(5), done)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

pub(crate) fn is_asked(event: &RunEvent) -> bool {
    matches!(event, RunEvent::PermissionAsked { .. })
}

pub(crate) fn is_finished(event: &RunEvent) -> bool {
    matches!(event, RunEvent::Finished { .. })
}

pub(crate) fn is_started(event: &RunEvent) -> bool {
    matches!(event, RunEvent::Started { .. })
}

pub(crate) fn run_id(id: &str) -> RunId {
    RunId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn answer(run: &str, allow: bool) -> PermissionAnswer {
    PermissionAnswer {
        run: run_id(run),
        prompt: 1,
        allow,
    }
}

pub(crate) fn started(session: &str) -> RunEvent {
    RunEvent::Started {
        session: AgentSession::try_from(session.to_owned()).unwrap(),
    }
}

pub(crate) fn text(text: &str) -> RunEvent {
    RunEvent::Text {
        text: text.to_owned(),
    }
}

pub(crate) fn tool_done(call: &str, failed: bool) -> RunEvent {
    RunEvent::ToolDone {
        call: call.to_owned(),
        failed,
    }
}

pub(crate) const fn ended(outcome: Outcome) -> RunEvent {
    RunEvent::Finished { outcome }
}
