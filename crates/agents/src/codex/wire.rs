//! The JSON-RPC lines `codex app-server` exchanges, as far as the driver needs them.

use serde::Deserialize;
use serde_json::{Value, json};
use weacon_protocol::{AccessLevel, AgentSession, App, RunEvent};

use crate::wire::{Asked, Effect, UNSUPPORTED};

const METHOD_NOT_FOUND: i64 = -32601;

/// Codex's side of a run: its thread, its turn, and the requests still waiting for answers.
pub(crate) struct Wire {
    client: Value,
    resume: Option<AgentSession>,
    read_only: bool,
    last_id: u64,
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

// The tool names a command and a file change go by, in prompts and in tool calls alike.
const COMMAND: &str = "command";
const FILE_CHANGE: &str = "file_change";

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
    /// A wire that opens a new thread, or resumes `resume`, introducing itself as `app`; with
    /// read-only `access` the thread runs in Codex's read-only sandbox.
    pub(crate) fn new(resume: Option<AgentSession>, app: &App, access: AccessLevel) -> Self {
        Self {
            client: json!({ "name": app.name, "title": "Weacon", "version": app.version }),
            resume,
            read_only: access == AccessLevel::ReadOnly,
            last_id: 0,
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
        self.last_id += 1;
        self.waiting.push((self.last_id, waiting));
        Effect::Send(json!({ "id": self.last_id, "method": method, "params": params }))
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
            (Waiting::Turn, None) => self.finished(false),
            // A turn's id comes from `turn/started`, which a late reply can't undo.
            (Waiting::Turn, Some(_)) | (Waiting::Interrupt, _) => Vec::new(),
        }
    }

    // A person answers every approval, and anything not known to be safe asks first.
    fn open_thread(&mut self) -> Effect {
        let (method, mut params) = match &self.resume {
            Some(session) => (
                "thread/resume",
                json!({
                    "threadId": session.as_str(),
                    "approvalPolicy": "untrusted",
                    "approvalsReviewer": "user",
                }),
            ),
            None => (
                "thread/start",
                json!({ "approvalPolicy": "untrusted", "approvalsReviewer": "user" }),
            ),
        };
        if self.read_only
            && let Some(params) = params.as_object_mut()
        {
            params.insert("sandbox".to_owned(), json!("read-only"));
        }
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

    fn finished(&mut self, ok: bool) -> Vec<Effect> {
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
            "turn/completed" => self
                .finished(turn_field(params, "status").is_some_and(|status| status == "completed")),
            "item/started" => item(params).and_then(started).into_iter().collect(),
            "item/completed" => item(params).and_then(completed).into_iter().collect(),
            _ => Vec::new(),
        }
    }
}

pub(crate) fn answer(request: &Value, allow: bool) -> Vec<Effect> {
    let decision = if allow { "accept" } else { "decline" };
    vec![Effect::Send(
        json!({ "id": request, "result": { "decision": decision } }),
    )]
}

fn asked(id: Value, method: &str, params: Value) -> Vec<Effect> {
    let tool = match method {
        "item/commandExecution/requestApproval" => COMMAND,
        "item/fileChange/requestApproval" => FILE_CHANGE,
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
    Item::deserialize(params.get("item")?).ok()
}

fn started(item: Item) -> Option<Effect> {
    let (call, tool, input) = match item {
        Item::CommandExecution { id, command, .. } => (id, COMMAND, json!({ "command": command })),
        Item::FileChange { id, changes, .. } => (id, FILE_CHANGE, json!({ "changes": changes })),
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
        self, Done, FakeRun, answer, app, ends_as_recorded, finished, is_asked, is_finished,
        is_started, next, started, text, tool_done, until,
    };
    use crate::wire::Wire as RunWire;

    const THREAD: &str = "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b";

    fn fake(recording: &str, resume: Option<&str>) -> (FakeRun, Done) {
        let resume = resume.map(|id| AgentSession::try_from(id.to_owned()).unwrap());
        let wire = Wire::new(resume, &app(), AccessLevel::AskFirst);
        test_support::fake(RunWire::Codex(wire), recording)
    }

    fn failed() -> RunEvent {
        finished(Outcome::Failed {
            code: code!("agent.turn_failed"),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_thread_runs_a_turn_and_an_approval_goes_out_and_back() {
        let (mut run, done) = fake(include_str!("../../sessions/codex/approval.replay"), None);

        run.send("Run the parser tests").await.unwrap();
        let events = until(&mut run, is_asked).await;
        assert_eq!(
            events.get(..2),
            Some(
                [
                    started(THREAD),
                    RunEvent::ToolCall {
                        call: "item-1".to_owned(),
                        tool: "command".to_owned(),
                        input: json!({ "command": "cargo test -p parser" }),
                    },
                ]
                .as_slice()
            )
        );
        assert!(matches!(
            events.get(2),
            Some(RunEvent::PermissionAsked { prompt: 1, tool, input, .. })
                if tool == "command" && input["command"] == "cargo test -p parser"
        ));
        run.answer(&answer("run-1", true)).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [
                tool_done("item-1", false),
                text("All 12 tests pass."),
                finished(Outcome::Completed),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_waits_for_its_turn_and_withdraws_waiting_prompts() {
        let (mut run, done) = fake(
            include_str!("../../sessions/codex/interrupt.replay"),
            Some(THREAD),
        );

        run.send("Rewrite the lexer").await.unwrap();
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
                    finished(Outcome::Interrupted),
                ]
                .as_slice()
            )
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_turn_takes_its_interrupt_along_and_a_declined_change_fails() {
        let (mut run, done) = fake(include_str!("../../sessions/codex/decline.replay"), None);

        run.send("Tidy the readme").await.unwrap();
        assert_eq!(until(&mut run, is_started).await, [started(THREAD)]);
        run.interrupt().await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [finished(Outcome::Interrupted)]
        );
        run.send("Tidy the readme").await.unwrap();
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
        let (mut run, done) = fake(include_str!("../../sessions/codex/no_thread.replay"), None);

        run.send("Explain the build").await.unwrap();
        assert_eq!(until(&mut run, is_finished).await, [failed()]);
        run.send("Explain the build").await.unwrap();
        assert_eq!(next(&mut run).await, Some(failed()));
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_prompt_interrupted_before_the_thread_opens_is_never_sent() {
        let (mut run, done) = fake(
            include_str!("../../sessions/codex/early_interrupt.replay"),
            None,
        );

        run.send("Bump the version").await.unwrap();
        run.interrupt().await.unwrap();
        assert_eq!(next(&mut run).await, Some(finished(Outcome::Interrupted)));
        assert_eq!(next(&mut run).await, Some(started(THREAD)));
        ends_as_recorded(run, done).await;
    }

    #[test]
    fn a_read_only_thread_asks_for_the_read_only_sandbox_and_others_dont() {
        let resume = AgentSession::try_from(THREAD.to_owned()).unwrap();
        let params = |resume: Option<AgentSession>, access| {
            let Effect::Send(line) = Wire::new(resume, &app(), access).open_thread() else {
                panic!("nothing sent");
            };
            line["params"]["sandbox"].clone()
        };

        let read_only = AccessLevel::ReadOnly;
        assert_eq!(params(None, read_only), json!("read-only"));
        assert_eq!(params(Some(resume.clone()), read_only), json!("read-only"));
        for access in [AccessLevel::AskFirst, AccessLevel::Full] {
            assert_eq!(params(None, access), Value::Null);
            assert_eq!(params(Some(resume.clone()), access), Value::Null);
        }
    }
}
