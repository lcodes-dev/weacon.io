//! A chat engine over fake agents, for tests.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use weacon_observability::Fault;
use weacon_protocol::{
    ChatAction, ChatAnswerKind, ChatCommand, ChatEvent, ChatId, ChatQuery, ChatQueryKind, Message,
    Outcome, PermissionAnswer, RequestId, RunEvent, RunId, WorkspaceId, code,
};
use weacon_testing::{Records, TempDir, recording_logger};

use super::engine::{Chats, Engine, Host, Start};
use super::feed::Client;
use crate::runs::Runs;
use crate::runs::task::Agent;
use crate::workspaces::Workspaces;

/// What a fake agent was told.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Told {
    Send(String),
    Answer(PermissionAnswer),
    Interrupt,
}

/// Like the driver's run, it ends a turn cut off by its exit as unknown.
pub(crate) struct FakeAgent {
    says: mpsc::UnboundedReceiver<RunEvent>,
    told: mpsc::UnboundedSender<Told>,
    on_turn: bool,
}

impl Agent for FakeAgent {
    async fn send(&mut self, prompt: &str) -> Result<(), Fault> {
        self.on_turn = true;
        drop(self.told.send(Told::Send(prompt.to_owned())));
        Ok(())
    }

    async fn answer(&mut self, answer: &PermissionAnswer) -> Result<(), Fault> {
        drop(self.told.send(Told::Answer(answer.clone())));
        Ok(())
    }

    async fn interrupt(&mut self) -> Result<(), Fault> {
        drop(self.told.send(Told::Interrupt));
        Ok(())
    }

    async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
        let said = self.says.recv().await;
        let cut_off = said.is_none() && self.on_turn;
        if cut_off || matches!(said, Some(RunEvent::Finished { .. })) {
            self.on_turn = false;
        }
        let outcome = Outcome::Unknown;
        Ok(said.or_else(|| cut_off.then_some(RunEvent::Finished { outcome })))
    }
}

/// The test's end of a started fake agent; dropping `say` makes the agent exit.
#[derive(Debug)]
pub(crate) struct Started {
    pub(crate) start: Start,
    pub(crate) say: mpsc::UnboundedSender<RunEvent>,
    pub(crate) told: mpsc::UnboundedReceiver<Told>,
}

pub(crate) struct Bench {
    pub(crate) chats: Chats,
    pub(crate) started: mpsc::UnboundedReceiver<Started>,
    /// While set, no agent can be started.
    pub(crate) broken: Arc<AtomicBool>,
    pub(crate) records: Records,
    _dir: TempDir,
}

/// An engine with its state in a new folder, running on the current runtime.
pub(crate) fn bench() -> Bench {
    let (bench, engine) = parts();
    tokio::spawn(engine.run());
    bench
}

/// As [`bench`], with the engine for the test to drive by hand.
pub(crate) fn parts() -> (Bench, Engine<FakeAgent>) {
    let dir = TempDir::new().unwrap();
    let (starts, started) = mpsc::unbounded_channel();
    let broken = Arc::new(AtomicBool::new(false));
    let fails = Arc::clone(&broken);
    let host = Host {
        start: Arc::new(move |start: &Start| {
            if fails.load(Ordering::SeqCst) {
                return Err(Fault::new(
                    code!("agent.not_started"),
                    "The agent couldn't start",
                ));
            }
            let (say, says) = mpsc::unbounded_channel();
            let (told_by, told) = mpsc::unbounded_channel();
            let start = start.clone();
            drop(starts.send(Started { start, say, told }));
            Ok(FakeAgent {
                says,
                told: told_by,
                on_turn: false,
            })
        }),
        now: || 1000,
    };
    let (log, records) = recording_logger();
    let runs = Runs::load(&dir).unwrap();
    let (chats, engine) = Engine::new(runs, Workspaces::new(&dir, None), host, log);
    let bench = Bench {
        chats,
        started,
        broken,
        records,
        _dir: dir,
    };
    (bench, engine)
}

pub(crate) fn workspace() -> WorkspaceId {
    WorkspaceId::try_from("w-1".to_owned()).unwrap()
}

pub(crate) fn chat(id: &str) -> ChatId {
    ChatId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn run(id: &str) -> RunId {
    RunId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn command(request: &str, chat_id: &str, action: ChatAction) -> ChatCommand {
    ChatCommand {
        request: RequestId::try_from(request.to_owned()).unwrap(),
        workspace: workspace(),
        chat: chat(chat_id),
        action,
        by: None,
    }
}

pub(crate) fn prompt(request: &str, text: &str) -> ChatCommand {
    let text = text.to_owned();
    command(
        request,
        "c-1",
        ChatAction::Prompt {
            text,
            repo: None,
            machine: None,
            agent: None,
        },
    )
}

pub(crate) fn query(chat_id: &str, query: ChatQueryKind) -> ChatQuery {
    ChatQuery {
        workspace: workspace(),
        chat: chat(chat_id),
        query,
    }
}

/// A client and what the engine pushes to it, with room for `queue` messages.
pub(crate) fn client(queue: usize) -> (Client, mpsc::Receiver<Message>) {
    let (push, pushed) = mpsc::channel(queue);
    (Client { push }, pushed)
}

/// What `received` gives within a few seconds, or `None`, so a test that waits fails instead
/// of hanging.
pub(crate) async fn soon<T>(received: impl Future<Output = Option<T>>) -> Option<T> {
    tokio::time::timeout(Duration::from_secs(5), received)
        .await
        .ok()
        .flatten()
}

/// The next `count` messages pushed to a client, each in brief.
pub(crate) async fn pushed(from: &mut mpsc::Receiver<Message>, count: usize) -> Vec<String> {
    let mut briefs = Vec::new();
    for _ in 0..count {
        let message = tokio::time::timeout(Duration::from_secs(5), from.recv()).await;
        briefs.push(message.map_or_else(
            |_| "nothing".to_owned(),
            |message| message.as_ref().map_or_else(|| "closed".to_owned(), brief),
        ));
    }
    briefs
}

/// A pushed message in a few words, such as `2 receipt r-1 accepted`.
pub(crate) fn brief(message: &Message) -> String {
    if let Message::Chat(update) = message {
        return format!("{} {}", update.entry.seq, event(&update.entry.event));
    }
    if let Message::Run(update) = message {
        return format!("live {} {}", update.run.as_str(), said(&update.event));
    }
    if let Message::ChatAnswer(answer) = message {
        let lost = matches!(answer.answer, ChatAnswerKind::EventsLost);
        return if lost { "events lost" } else { "an answer" }.to_owned();
    }
    "another message".to_owned()
}

fn event(event: &ChatEvent) -> String {
    match event {
        ChatEvent::Prompt { request, text, .. } => format!("prompt {} {text}", request.as_str()),
        ChatEvent::Receipt { receipt } => {
            format!(
                "receipt {} {}",
                receipt.request.as_str(),
                outcome(&receipt.outcome)
            )
        }
        ChatEvent::Run { update } => format!("{} {}", update.run.as_str(), said(&update.event)),
        ChatEvent::RunStatus { status } => {
            format!("{} is {:?}", status.run.as_str(), status.state)
        }
        ChatEvent::Newer => "newer".to_owned(),
    }
}

fn said(event: &RunEvent) -> String {
    match event {
        RunEvent::Started { .. } => "started".to_owned(),
        RunEvent::TextDelta { text } => format!("delta {text}"),
        RunEvent::Text { text } => format!("text {text}"),
        RunEvent::ToolCall { tool, .. } => format!("calls {tool}"),
        RunEvent::ToolDone { .. } => "tool done".to_owned(),
        RunEvent::PermissionAsked { prompt, .. } => format!("asks {prompt}"),
        RunEvent::PermissionWithdrawn { prompt, .. } => format!("withdraws {prompt}"),
        RunEvent::Finished { outcome: ended } => format!("finished {}", outcome(ended)),
        RunEvent::Newer => "newer".to_owned(),
    }
}

fn outcome(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Accepted => "accepted".to_owned(),
        Outcome::Completed => "completed".to_owned(),
        Outcome::Interrupted => "interrupted".to_owned(),
        Outcome::Failed { code } => format!("failed {}", code.as_str()),
        Outcome::Unknown => "unknown".to_owned(),
        Outcome::Newer => "newer".to_owned(),
    }
}
