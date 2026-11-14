//! A chat engine over fake agents, for tests.

use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, AgentSession, ChatAction, ChatAnswerKind, ChatCommand, ChatEvent, ChatId, ChatQuery,
    ChatQueryKind, Message, Outcome, PermissionAnswer, RequestId, RunEvent, RunId, WorkspaceId,
    code,
};
use weacon_testing::{Records, TempDir, recording_logger};

use super::engine::agent::Agent;
use super::engine::{Chats, Engine, Host, RunAs};
use super::feed::Client;
use crate::runs::{ChatKey, Runs};
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
    pub(crate) run_as: RunAs,
    pub(crate) say: mpsc::UnboundedSender<RunEvent>,
    pub(crate) told: mpsc::UnboundedReceiver<Told>,
}

pub(crate) struct Bench {
    pub(crate) chats: Chats,
    pub(crate) started: mpsc::UnboundedReceiver<Started>,
    /// While set, no agent can be started.
    pub(crate) broken: Arc<AtomicBool>,
    /// The next agent's start blocks, as a slow one does, until this is sent to.
    gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>,
    pub(crate) records: Records,
    _dir: TempDir,
}

impl Bench {
    /// Holds up the next agent's start until the returned sender is sent to or dropped.
    pub(crate) fn hold_next_start(&self) -> std::sync::mpsc::Sender<()> {
        let (release, held) = std::sync::mpsc::channel();
        if let Ok(mut gate) = self.gate.lock() {
            *gate = Some(held);
        }
        release
    }
}

/// An engine with its state in a new folder, running on the current runtime.
pub(crate) fn bench() -> Bench {
    let (bench, engine) = parts();
    tokio::spawn(engine.serve());
    bench
}

/// As [`bench`], with the engine for the test to drive by hand.
pub(crate) fn parts() -> (Bench, Engine<FakeAgent>) {
    let dir = TempDir::new().unwrap();
    let (starts, started) = mpsc::unbounded_channel();
    let broken = Arc::new(AtomicBool::new(false));
    let fails = Arc::clone(&broken);
    let gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>> = Arc::new(Mutex::new(None));
    let waits = Arc::clone(&gate);
    let host = Host {
        launch: Arc::new(move |run_as: &RunAs| {
            // Sent to or dropped, the start goes on either way.
            let held = waits.lock().ok().and_then(|mut gate| gate.take());
            let _ = held.map(|held| held.recv());
            if fails.load(Ordering::SeqCst) {
                return Err(Fault::new(
                    code!("agent.not_started"),
                    "The agent couldn't start",
                ));
            }
            let (say, says) = mpsc::unbounded_channel();
            let (told_by, told) = mpsc::unbounded_channel();
            let run_as = run_as.clone();
            drop(starts.send(Started { run_as, say, told }));
            Ok(FakeAgent {
                says,
                told: told_by,
                on_turn: false,
            })
        }),
        now: || 1_000_000,
    };
    let (log, records) = recording_logger();
    let runs = Runs::load(&dir).unwrap();
    let (chats, engine) = Engine::new(runs, Workspaces::new(&dir, None), host, log);
    let bench = Bench {
        chats,
        started,
        broken,
        gate,
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

/// Chat `c-1` in `w-1`.
pub(crate) fn key() -> ChatKey {
    ChatKey {
        workspace: workspace(),
        chat: chat("c-1"),
    }
}

pub(crate) fn run(id: &str) -> RunId {
    RunId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn request(id: &str) -> RequestId {
    RequestId::try_from(id.to_owned()).unwrap()
}

pub(crate) fn session() -> AgentSession {
    AgentSession::try_from("0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d".to_owned()).unwrap()
}

pub(crate) fn text(text: &str) -> RunEvent {
    RunEvent::Text {
        text: text.to_owned(),
    }
}

/// Run `run_id` asking a person whether it may run `ls`.
pub(crate) fn asks(run_id: &str, prompt: u32) -> RunEvent {
    RunEvent::PermissionAsked {
        run: run(run_id),
        prompt,
        tool: "Bash".to_owned(),
        input: serde_json::json!({ "command": "ls" }),
    }
}

/// Request `request` allowing what run `run_id` asked in prompt `prompt`, in chat `chat_id`.
pub(crate) fn allow(request: &str, chat_id: &str, run_id: &str, prompt: u32) -> ChatCommand {
    let answer = PermissionAnswer {
        run: run(run_id),
        prompt,
        allow: true,
    };
    command(request, chat_id, ChatAction::Permission { answer })
}

/// A prompt as chat `c-1`'s log holds it.
pub(crate) fn logged_prompt(request_id: &str, text: &str) -> ChatEvent {
    ChatEvent::Prompt {
        request: request(request_id),
        text: text.to_owned(),
        repo: None,
        agent: None,
    }
}

pub(crate) fn command(request_id: &str, chat_id: &str, action: ChatAction) -> ChatCommand {
    ChatCommand {
        request: request(request_id),
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

/// A prompt under `request` that names `agent`.
pub(crate) fn prompt_naming(request: &str, text: &str, agent: AgentKind) -> ChatCommand {
    let mut asked = prompt(request, text);
    if let ChatAction::Prompt { agent: named, .. } = &mut asked.action {
        *named = Some(agent);
    }
    asked
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

/// Passes over the next `count` pushed messages, failing unless each one arrives.
pub(crate) async fn skip(from: &mut mpsc::Receiver<Message>, count: usize) {
    let briefs = pushed(from, count).await;
    let missing = |brief: &String| brief == "nothing" || brief == "closed";
    assert!(!briefs.iter().any(missing), "{briefs:?}");
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
        ChatEvent::OutsidePrompt { text } => format!("outside prompt {text}"),
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
        RunEvent::PermissionAnswered { answer } => format!("answered {}", answer.prompt),
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
