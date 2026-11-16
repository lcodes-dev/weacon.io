//! The JSON-RPC lines `codex app-server` exchanges, as far as the driver needs them.

use serde::Deserialize;
use serde_json::{Value, json};
use weacon_protocol::{AccessLevel, AgentSession, App, RunEvent, Wants};

use crate::wire::{Asked, Effect, UNSUPPORTED};

const METHOD_NOT_FOUND: i64 = -32601;

/// Codex's side of a run: its thread, its turn, and the requests still waiting for answers.
pub(crate) struct Wire {
    client: Value,
    resume: Option<AgentSession>,
    sandbox: &'static str,
    next_id: u64,
    waiting: Vec<(u64, Waiting)>,
    thread: Option<String>,
    turn: Option<String>,
    // A prompt sent before the thread opened.
    queued: Option<String>,
    interrupt_pending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Waiting {
    Initialize,
    Thread,
    Turn,
    Interrupt,
}

/// A request, a notification or a response: which one is told by `id` and `method`.
#[derive(Deserialize)]
struct Incoming {
    id: Option<Value>,
    method: Option<String>,
    #[serde(default)]
    params: Value,
    result: Option<Value>,
}

#[derive(Deserialize)]
struct ItemNote {
    item: Item,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Item {
    AgentMessage {
        text: String,
    },
    CommandExecution {
        id: String,
        command: String,
        status: String,
    },
    FileChange {
        id: String,
        changes: Value,
        status: String,
    },
    #[serde(other)]
    Other,
}

impl Wire {
    /// A wire that opens a new thread, or resumes `resume`, introducing itself as `app`, in the
    /// sandbox `access` allows.
    pub(crate) fn new(resume: Option<AgentSession>, app: &App, access: AccessLevel) -> Self {
        Self {
            client: json!({ "name": app.name, "title": "Weacon", "version": app.version }),
            resume,
            sandbox: sandbox(access),
            next_id: 0,
            waiting: Vec::new(),
            thread: None,
            turn: None,
            queued: None,
            interrupt_pending: false,
        }
    }

    pub(crate) fn opening(&mut self) -> Vec<Effect> {
        let params = json!({ "clientInfo": self.client });
        vec![self.request(Waiting::Initialize, "initialize", &params)]
    }

    pub(crate) fn prompt(&mut self, text: &str) -> Vec<Effect> {
        let Some(thread) = self.thread.clone() else {
            if !self.opening_thread() {
                return vec![Effect::Finished { ok: false }];
            }
            self.queued = Some(text.to_owned());
            return Vec::new();
        };
        let input = json!([{ "type": "text", "text": text, "text_elements": [] }]);
        let params = json!({ "threadId": thread, "input": input });
        vec![self.request(Waiting::Turn, "turn/start", &params)]
    }

    pub(crate) fn interrupt(&mut self) -> Vec<Effect> {
        // A prompt that never went out ends here, as interrupted.
        if self.queued.take().is_some() {
            return vec![Effect::Finished { ok: false }];
        }
        self.interrupt_pending = true;
        self.interrupt_turn()
    }

    pub(crate) fn read(&mut self, line: &[u8]) -> Vec<Effect> {
        let Ok(incoming) = serde_json::from_slice::<Incoming>(line) else {
            return Vec::new();
        };
        match (incoming.id, incoming.method) {
            (Some(id), Some(method)) => asked(id, &method, incoming.params),
            (None, Some(method)) => self.notified(&method, &incoming.params),
            (Some(id), None) => self.answered(&id, incoming.result),
            (None, None) => Vec::new(),
        }
    }

    fn request(&mut self, waiting: Waiting, method: &str, params: &Value) -> Effect {
        self.next_id += 1;
        self.waiting.push((self.next_id, waiting));
        Effect::Send(json!({ "id": self.next_id, "method": method, "params": params }))
    }

    fn opening_thread(&self) -> bool {
        self.waiting
            .iter()
            .any(|(_, waiting)| matches!(waiting, Waiting::Initialize | Waiting::Thread))
    }

    // A response without a result carries an error instead.
    fn answered(&mut self, id: &Value, result: Option<Value>) -> Vec<Effect> {
        let Some(at) = self
            .waiting
            .iter()
            .position(|(sent, _)| id.as_u64() == Some(*sent))
        else {
            return Vec::new();
        };
        let (_, waiting) = self.waiting.swap_remove(at);
        match (waiting, result) {
            (Waiting::Initialize, Some(_)) => {
                vec![
                    Effect::Send(json!({ "method": "initialized" })),
                    self.open_thread(),
                ]
            }
            (Waiting::Thread, Some(result)) => self.thread_opened(&result),
            (Waiting::Initialize | Waiting::Thread, None) => self.give_up(),
            (Waiting::Turn, None) => self.ended(false),
            // A turn's id comes from `turn/started`, which a late reply can't undo.
            (Waiting::Turn, Some(_)) | (Waiting::Interrupt, _) => Vec::new(),
        }
    }

    // A person answers every approval, and with `untrusted` every command and patch asks first.
    // No `cwd`: given one, Codex marks the folder a trusted project and loads its own config.
    fn open_thread(&mut self) -> Effect {
        let mut params = json!({
            "approvalPolicy": "untrusted",
            "approvalsReviewer": "user",
            "sandbox": self.sandbox,
        });
        let method = if let Some(session) = &self.resume
            && let Some(params) = params.as_object_mut()
        {
            params.insert("threadId".to_owned(), json!(session.as_str()));
            "thread/resume"
        } else {
            "thread/start"
        };
        self.request(Waiting::Thread, method, &params)
    }

    fn thread_opened(&mut self, result: &Value) -> Vec<Effect> {
        let Some(id) = result.pointer("/thread/id").and_then(Value::as_str) else {
            return self.give_up();
        };
        self.thread = Some(id.to_owned());
        let mut effects: Vec<_> = AgentSession::try_from(id.to_owned())
            .ok()
            .map(Effect::Started)
            .into_iter()
            .collect();
        if let Some(text) = self.queued.take() {
            effects.extend(self.prompt(&text));
        }
        effects
    }

    fn give_up(&mut self) -> Vec<Effect> {
        self.queued
            .take()
            .map(|_| Effect::Finished { ok: false })
            .into_iter()
            .collect()
    }

    fn interrupt_turn(&mut self) -> Vec<Effect> {
        let (true, Some(thread), Some(turn)) = (
            self.interrupt_pending,
            self.thread.clone(),
            self.turn.clone(),
        ) else {
            return Vec::new();
        };
        self.interrupt_pending = false;
        let params = json!({ "threadId": thread, "turnId": turn });
        vec![self.request(Waiting::Interrupt, "turn/interrupt", &params)]
    }

    fn ended(&mut self, ok: bool) -> Vec<Effect> {
        self.turn = None;
        self.interrupt_pending = false;
        vec![Effect::Finished { ok }]
    }

    // Notices from other threads, such as a sub-agent's, aren't this run's; approvals are
    // asked on any thread, so their resolutions are effects from any thread too.
    fn notified(&mut self, method: &str, params: &Value) -> Vec<Effect> {
        if method == "serverRequest/resolved" {
            return params
                .get("requestId")
                .map(|request| Effect::Withdrawn {
                    request: request.clone(),
                })
                .into_iter()
                .collect();
        }
        if params.get("threadId").and_then(Value::as_str) != self.thread.as_deref() {
            return Vec::new();
        }
        match method {
            "turn/started" => {
                self.turn = turn_field(params, "id");
                self.interrupt_turn()
            }
            "turn/completed" => {
                self.ended(turn_field(params, "status").is_some_and(|status| status == "completed"))
            }
            "item/started" => item(params).and_then(started).into_iter().collect(),
            "item/completed" => item(params).and_then(completed).into_iter().collect(),
            // The whole message follows in its `item/completed`, which replaces these.
            "item/agentMessage/delta" => text_delta(params).into_iter().collect(),
            _ => Vec::new(),
        }
    }
}

// Writes stay in the folder Codex starts in, unless a person lets a command out.
const fn sandbox(access: AccessLevel) -> &'static str {
    match access {
        AccessLevel::AskFirst | AccessLevel::Full => "workspace-write",
        AccessLevel::Off | AccessLevel::ReadOnly | AccessLevel::Newer => "read-only",
    }
}

pub(crate) fn answer(request: &Value, allow: bool) -> Vec<Effect> {
    let decision = if allow { "accept" } else { "decline" };
    vec![Effect::Send(
        json!({ "id": request, "result": { "decision": decision } }),
    )]
}

/// What a prompt shows of the request `input`: a command; a file change's doesn't say what
/// changes.
pub(crate) fn wants(input: &Value) -> Option<Wants> {
    input.get("command")?.as_str().map(Wants::of)
}

fn asked(id: Value, method: &str, params: Value) -> Vec<Effect> {
    let tool = match method {
        "item/commandExecution/requestApproval" => "command",
        "item/fileChange/requestApproval" => "file_change",
        _ => {
            let error = json!({ "code": METHOD_NOT_FOUND, "message": UNSUPPORTED });
            return vec![Effect::Send(json!({ "id": id, "error": error }))];
        }
    };
    vec![Effect::Asked(Asked {
        request: id,
        tool: tool.to_owned(),
        input: params,
    })]
}

fn turn_field(value: &Value, field: &str) -> Option<String> {
    value.get("turn")?.get(field)?.as_str().map(str::to_owned)
}

fn item(params: &Value) -> Option<Item> {
    ItemNote::deserialize(params).ok().map(|note| note.item)
}

fn text_delta(params: &Value) -> Option<Effect> {
    let text = params.get("delta")?.as_str()?.to_owned();
    Some(Effect::Event(RunEvent::TextDelta { text }))
}

fn started(item: Item) -> Option<Effect> {
    let (call, tool, input) = match item {
        Item::CommandExecution { id, command, .. } => {
            (id, "command", json!({ "command": command }))
        }
        Item::FileChange { id, changes, .. } => (id, "file_change", json!({ "changes": changes })),
        Item::AgentMessage { .. } | Item::Other => return None,
    };
    Some(Effect::Event(RunEvent::ToolCall {
        call,
        tool: tool.to_owned(),
        input,
    }))
}

fn completed(item: Item) -> Option<Effect> {
    let event = match item {
        Item::AgentMessage { text } => RunEvent::Text { text },
        Item::CommandExecution { id, status, .. } | Item::FileChange { id, status, .. } => {
            RunEvent::ToolDone {
                call: id,
                failed: status != "completed",
            }
        }
        Item::Other => return None,
    };
    Some(Effect::Event(event))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::{Outcome, code};

    use super::*;
    use crate::test_support::{
        self, Done, FakeRun, answer, app, delta, ended, ends_as_recorded, is_asked, is_finished,
        is_started, next, started, text, tool_call, tool_done, until,
    };
    use crate::wire::Wire as RunWire;

    const THREAD: &str = "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b";

    fn fake(recording: &str, resume: Option<&str>) -> (FakeRun, Done) {
        let resume = resume.map(|id| AgentSession::try_from(id.to_owned()).unwrap());
        let wire = Wire::new(resume, &app(), AccessLevel::AskFirst);
        test_support::fake(RunWire::Codex(wire), recording)
    }

    fn failed() -> RunEvent {
        ended(Outcome::Failed {
            code: code!("agent.turn_failed"),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_thread_runs_a_turn_and_an_approval_goes_out_and_back() {
        let (mut run, done) = fake(include_str!("../../replays/codex/approval.replay"), None);

        run.send("Run the parser tests", None).await.unwrap();
        let events = until(&mut run, is_asked).await;
        assert_eq!(
            events.get(..2),
            Some(
                [
                    started(THREAD),
                    tool_call(
                        "item-1",
                        "command",
                        json!({ "command": "cargo test -p parser" })
                    ),
                ]
                .as_slice()
            )
        );
        assert!(matches!(
            events.get(2),
            Some(RunEvent::PermissionAsked { prompt: 1, tool, input, wants, .. })
                if tool == "command" && input["command"] == "cargo test -p parser"
                    && *wants == Some(Wants::of("cargo test -p parser"))
        ));
        // A file change's request names no file, so its prompt shows nothing.
        assert_eq!(wants(&json!({ "itemId": "item-5" })), None);
        run.answer(&answer("run-1", true)).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [
                delta("All 12"),
                tool_done("item-1", false),
                text("All 12 tests pass."),
                ended(Outcome::Completed),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn text_comes_in_pieces_before_each_whole_message_with_a_command_between() {
        let (mut run, done) = fake(include_str!("../../replays/codex/partial.replay"), None);

        run.send("Run the parser tests", None).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [
                started(THREAD),
                delta("I'll run"),
                delta(" them."),
                text("I'll run them."),
                tool_call(
                    "item-2",
                    "command",
                    json!({ "command": "cargo test -p parser" })
                ),
                tool_done("item-2", false),
                delta("All 12"),
                delta(" tests"),
                delta(" pass."),
                text("All 12 tests pass."),
                ended(Outcome::Completed),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn partial_output_the_driver_doesnt_know_is_skipped_and_the_turn_goes_on() {
        let recording = include_str!("../../replays/codex/partial_unknown.replay");
        let (mut run, done) = fake(recording, None);

        run.send("Explain the build", None).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [
                started(THREAD),
                delta("Cargo"),
                delta(" builds it."),
                text("Cargo builds it."),
                ended(Outcome::Completed),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    // Pieces are shown but never kept, so a turn cut off before its whole message has no text.
    #[tokio::test(start_paused = true)]
    async fn a_turn_cut_off_mid_message_gave_only_its_pieces() {
        let recording = include_str!("../../replays/codex/partial_cut_off.replay");
        let (mut run, done) = fake(recording, None);

        run.send("Bump the version", None).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [
                started(THREAD),
                delta("Bumping"),
                delta(" it"),
                ended(Outcome::Unknown),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_waits_for_its_turn_and_withdraws_waiting_prompts() {
        let (mut run, done) = fake(
            include_str!("../../replays/codex/interrupt.replay"),
            Some(THREAD),
        );

        run.send("Rewrite the lexer", None).await.unwrap();
        assert_eq!(until(&mut run, is_started).await, [started(THREAD)]);
        run.interrupt().await.unwrap();
        let events = until(&mut run, is_finished).await;
        assert!(matches!(
            events.first(),
            Some(RunEvent::PermissionAsked { prompt: 1, input, .. }) if input["command"] == "rm -rf target"
        ));
        assert_eq!(
            events.get(1..),
            Some(
                [
                    RunEvent::PermissionWithdrawn {
                        run: test_support::run_id("run-1"),
                        prompt: 1,
                    },
                    ended(Outcome::Interrupted),
                ]
                .as_slice()
            )
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_turn_takes_its_interrupt_along_and_a_declined_change_fails() {
        let (mut run, done) = fake(include_str!("../../replays/codex/decline.replay"), None);

        run.send("Tidy the readme", None).await.unwrap();
        assert_eq!(until(&mut run, is_started).await, [started(THREAD)]);
        run.interrupt().await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [ended(Outcome::Interrupted)]
        );
        run.send("Tidy the readme", None).await.unwrap();
        let events = until(&mut run, is_asked).await;
        assert!(matches!(
            events.first(),
            Some(RunEvent::ToolCall { call, tool, .. }) if call == "item-5" && tool == "file_change"
        ));
        run.answer(&answer("run-1", false)).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [tool_done("item-5", true), failed()]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_thread_every_prompt_fails() {
        let (mut run, done) = fake(include_str!("../../replays/codex/no_thread.replay"), None);

        run.send("Explain the build", None).await.unwrap();
        assert_eq!(until(&mut run, is_finished).await, [failed()]);
        run.send("Explain the build", None).await.unwrap();
        assert_eq!(next(&mut run).await, Some(failed()));
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_interrupted_before_the_thread_opens_is_never_sent() {
        let (mut run, done) = fake(
            include_str!("../../replays/codex/early_interrupt.replay"),
            None,
        );

        run.send("Bump the version", None).await.unwrap();
        run.interrupt().await.unwrap();
        assert_eq!(next(&mut run).await, Some(ended(Outcome::Interrupted)));
        assert_eq!(next(&mut run).await, Some(started(THREAD)));
        ends_as_recorded(run, done).await;
    }

    #[test]
    fn every_thread_asks_first_in_its_levels_sandbox_and_names_no_folder() {
        let resume = AgentSession::try_from(THREAD.to_owned()).unwrap();
        let opened = |resume: Option<AgentSession>, access| {
            let Effect::Send(line) = Wire::new(resume, &app(), access).open_thread() else {
                panic!("nothing sent");
            };
            (line["method"].clone(), line["params"].clone())
        };
        let params = |sandbox: &str| json!({ "approvalPolicy": "untrusted", "approvalsReviewer": "user", "sandbox": sandbox });

        for (access, sandbox) in [
            (AccessLevel::ReadOnly, "read-only"),
            (AccessLevel::Off, "read-only"),
            (AccessLevel::AskFirst, "workspace-write"),
            (AccessLevel::Full, "workspace-write"),
        ] {
            let mut resumed = params(sandbox);
            resumed["threadId"] = json!(THREAD);
            assert_eq!(
                opened(None, access),
                (json!("thread/start"), params(sandbox)),
                "{access:?}"
            );
            assert_eq!(
                opened(Some(resume.clone()), access),
                (json!("thread/resume"), resumed),
                "{access:?}"
            );
        }
    }
}
