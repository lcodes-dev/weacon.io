//! Helpers for the drivers' tests: a run over a replayed session, and the events it yields.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{BufReader, DuplexStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use weacon_protocol::{
    AccessLevel, AgentKind, AgentSession, App, Outcome, PermissionAnswer, RunEvent, RunId,
};
use weacon_testing::{Failure, replay};

use crate::Launch;
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

/// `lines` as a file of JSON lines.
pub(crate) fn jsonl(lines: &[Value]) -> String {
    lines
        .iter()
        .fold(String::new(), |file, line| file + &line.to_string() + "\n")
}

/// The next thing a terminal's program did, failing the test if nothing comes in time.
pub(crate) async fn next_event(events: &mut crate::Events) -> Option<crate::Event> {
    timeout(Duration::from_secs(30), events.next())
        .await
        .unwrap()
}

/// The first event that isn't output, with the output before it added to `seen`.
pub(crate) async fn after_output(
    events: &mut crate::Events,
    seen: &mut String,
) -> Option<crate::Event> {
    loop {
        match next_event(events).await {
            Some(crate::Event::Output(data)) => seen.push_str(&String::from_utf8_lossy(&data)),
            other => return other,
        }
    }
}

/// Output into `seen` until it holds `wanted`.
pub(crate) async fn output_until(events: &mut crate::Events, seen: &mut String, wanted: &str) {
    while !seen.contains(wanted) {
        match next_event(events).await {
            Some(crate::Event::Output(data)) => seen.push_str(&String::from_utf8_lossy(&data)),
            other => panic!("{other:?} before {wanted:?}, after {seen:?}"),
        }
    }
}

pub(crate) const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";
pub(crate) const OTHER_SESSION: &str = "1c8d7b63-4a2f-4e3b-8d9c-6f5a4b3c2d1e";

pub(crate) fn tool_call(call: &str, tool: &str, input: Value) -> RunEvent {
    RunEvent::ToolCall {
        call: call.to_owned(),
        tool: tool.to_owned(),
        input,
    }
}

/// A launch of `agent` running `program`, working in `dir`, with its config folder there.
pub(crate) fn launch(agent: AgentKind, program: impl Into<PathBuf>, dir: &Path) -> Launch {
    Launch {
        agent,
        program: program.into(),
        config_dir: dir.join("config"),
        cwd: dir.to_path_buf(),
        access: AccessLevel::AskFirst,
        mcp: None,
        plugin: None,
    }
}
