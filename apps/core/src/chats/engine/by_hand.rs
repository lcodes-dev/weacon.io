//! An engine driven by hand, for the engine's own tests.

use serde_json::json;
use tokio::sync::mpsc;
use weacon_observability::Fault;
use weacon_protocol::{
    AgentSession, ChatAction, ChatAnswerKind, ChatCommand, ChatEvent, ChatQueryKind, Message,
    Outcome, PermissionAnswer, RunEvent,
};

use super::agent::Heard;
use super::{Engine, PendingAnswer};
use crate::chats::test_support::{
    Bench, FakeAgent, Started, Told, client, command, key, parts, prompt, run, skip, soon,
};

/// An engine driven by hand, on its first turn, with a client watching the chat.
pub(super) struct OnTurn {
    pub(super) bench: Bench,
    pub(super) engine: Engine<FakeAgent>,
    pub(super) agent: Started,
    pub(super) pushes: mpsc::Receiver<Message>,
}

pub(super) async fn on_turn() -> OnTurn {
    let (mut bench, mut engine) = parts();
    let (client, mut pushes) = client(64);
    let subscribe = ChatQueryKind::Subscribe { after: 0 };
    engine.query(&key(), subscribe, &client).await.unwrap();
    engine.command(&key(), prompt("r-1", "Hi")).await.unwrap();
    skip(&mut pushes, 4).await;
    let agent = soon(bench.started.recv()).await.unwrap();
    OnTurn {
        bench,
        engine,
        agent,
        pushes,
    }
}

pub(super) async fn hear(engine: &mut Engine<FakeAgent>, event: RunEvent) -> Result<(), Fault> {
    engine
        .heard(&key(), &run("run-1"), Heard::Event(event))
        .await
}

pub(super) async fn latest(engine: &mut Engine<FakeAgent>) -> ChatEvent {
    let log = &engine.open(&key()).await.unwrap().log;
    let mut window = log.window(u64::MAX, 1).await.unwrap();
    window.pop().unwrap().event
}

pub(super) fn session() -> AgentSession {
    AgentSession::try_from("0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d".to_owned()).unwrap()
}

// A run on its turn, waiting for a person to answer its prompt 1.
pub(super) async fn on_ask() -> OnTurn {
    let mut on = on_turn().await;
    assert_eq!(
        soon(on.agent.told.recv()).await,
        Some(Told::Send("Hi".to_owned()))
    );
    let asks = RunEvent::PermissionAsked {
        run: run("run-1"),
        prompt: 1,
        tool: "Write".to_owned(),
        input: json!({}),
    };
    hear(&mut on.engine, asks).await.unwrap();
    on
}

// What the chat's snapshot says waits for a person.
pub(super) async fn waiting_in(engine: &mut Engine<FakeAgent>) -> Vec<u64> {
    let (anyone, _pushes) = client(1);
    let snapshot = engine.query(&key(), ChatQueryKind::Snapshot, &anyone).await;
    let Ok(Some(ChatAnswerKind::Snapshot { waiting, .. })) = snapshot else {
        panic!("no snapshot");
    };
    waiting
}

pub(super) fn allow(request: &str) -> (ChatCommand, PermissionAnswer) {
    let answer = PermissionAnswer {
        run: run("run-1"),
        prompt: 1,
        allow: true,
    };
    let action = ChatAction::Permission {
        answer: answer.clone(),
    };
    (command(request, "c-1", action), answer)
}

// An answer to prompt 1 whose receipt is written but which hasn't reached the run.
pub(super) async fn on_its_way(engine: &mut Engine<FakeAgent>) -> ChatCommand {
    let (answers, answer) = allow("r-2");
    let request = answers.request.clone();
    let answered = PendingAnswer {
        request: request.clone(),
        answer,
    };
    engine
        .open(&key())
        .await
        .unwrap()
        .pending_answers
        .push(answered);
    let receipt = Outcome::Completed;
    engine
        .commit_receipt(&key(), &request, receipt)
        .await
        .unwrap();
    answers
}
