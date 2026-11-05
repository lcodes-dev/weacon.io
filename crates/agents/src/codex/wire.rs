//! The JSON-RPC lines `codex app-server` exchanges, as far as the driver needs them.

use serde::Deserialize;
use serde_json::{Value, json};
use weacon_protocol::{AgentSession, App, RunEvent};

use crate::run::{Heard, UNSUPPORTED};

// JSON-RPC's code for a method the receiver doesn't have.
const NO_METHOD: i64 = -32601;

/// Codex's side of a run: its thread, its turn, and the requests still waiting for answers.
pub(crate) struct Wire {
    resume: Option<AgentSession>,
    next_id: u64,
    waiting: Vec<(u64, Waiting)>,
    thread: Option<String>,
    turn: Option<String>,
    // A prompt sent before the thread opened.
    queued: Option<String>,
    interrupt: bool,
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
    pub(crate) const fn new(resume: Option<AgentSession>) -> Self {
        Self {
            resume,
            next_id: 0,
            waiting: Vec::new(),
            thread: None,
            turn: None,
            queued: None,
            interrupt: false,
        }
    }

    pub(crate) fn opening(&mut self, app: &App) -> Vec<Heard> {
        let client = json!({ "name": app.name, "title": "Weacon", "version": app.version });
        vec![self.request(
            Waiting::Initialize,
            "initialize",
            &json!({ "clientInfo": client }),
        )]
    }

    pub(crate) fn prompt(&mut self, text: &str) -> Vec<Heard> {
        let Some(thread) = self.thread.clone() else {
            if !self.opening_thread() {
                return vec![Heard::Finished { ok: false }];
            }
            self.queued = Some(text.to_owned());
            return Vec::new();
        };
        let input = json!([{ "type": "text", "text": text, "text_elements": [] }]);
        let params = json!({ "threadId": thread, "input": input });
        vec![self.request(Waiting::Turn, "turn/start", &params)]
    }

    pub(crate) fn interrupt(&mut self) -> Vec<Heard> {
        // A prompt that never went out ends here, as interrupted.
        if self.queued.take().is_some() {
            return vec![Heard::Finished { ok: false }];
        }
        self.interrupt = true;
        self.interrupt_turn()
    }

    pub(crate) fn read(&mut self, line: &[u8]) -> Vec<Heard> {
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

    fn request(&mut self, waiting: Waiting, method: &str, params: &Value) -> Heard {
        self.next_id += 1;
        self.waiting.push((self.next_id, waiting));
        Heard::Send(json!({ "id": self.next_id, "method": method, "params": params }))
    }

    fn opening_thread(&self) -> bool {
        self.waiting
            .iter()
            .any(|(_, waiting)| matches!(waiting, Waiting::Initialize | Waiting::Thread))
    }

    // A response without a result carries an error instead.
    fn answered(&mut self, id: &Value, result: Option<Value>) -> Vec<Heard> {
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
                    Heard::Send(json!({ "method": "initialized" })),
                    self.open_thread(),
                ]
            }
            (Waiting::Thread, Some(result)) => self.thread_opened(&result),
            (Waiting::Turn, Some(result)) => {
                self.turn = turn_field(&result, "id");
                self.interrupt_turn()
            }
            (Waiting::Initialize | Waiting::Thread, None) => self.give_up(),
            (Waiting::Turn, None) => vec![Heard::Finished { ok: false }],
            (Waiting::Interrupt, _) => Vec::new(),
        }
    }

    fn open_thread(&mut self) -> Heard {
        match self.resume.clone() {
            Some(session) => {
                let params = json!({ "threadId": session.as_str() });
                self.request(Waiting::Thread, "thread/resume", &params)
            }
            None => self.request(Waiting::Thread, "thread/start", &json!({})),
        }
    }

    fn thread_opened(&mut self, result: &Value) -> Vec<Heard> {
        let Some(id) = result.pointer("/thread/id").and_then(Value::as_str) else {
            return self.give_up();
        };
        self.thread = Some(id.to_owned());
        let mut heard: Vec<_> = AgentSession::try_from(id.to_owned())
            .ok()
            .map(Heard::Started)
            .into_iter()
            .collect();
        if let Some(text) = self.queued.take() {
            heard.extend(self.prompt(&text));
        }
        heard
    }

    fn give_up(&mut self) -> Vec<Heard> {
        self.queued
            .take()
            .map(|_| Heard::Finished { ok: false })
            .into_iter()
            .collect()
    }

    fn interrupt_turn(&mut self) -> Vec<Heard> {
        let (true, Some(thread), Some(turn)) =
            (self.interrupt, self.thread.clone(), self.turn.clone())
        else {
            return Vec::new();
        };
        self.interrupt = false;
        let params = json!({ "threadId": thread, "turnId": turn });
        vec![self.request(Waiting::Interrupt, "turn/interrupt", &params)]
    }

    fn notified(&mut self, method: &str, params: &Value) -> Vec<Heard> {
        match method {
            "turn/started" => {
                self.turn = turn_field(params, "id");
                self.interrupt_turn()
            }
            "turn/completed" => {
                self.turn = None;
                self.interrupt = false;
                let ok = turn_field(params, "status").is_some_and(|status| status == "completed");
                vec![Heard::Finished { ok }]
            }
            "item/started" => item(params).and_then(started).into_iter().collect(),
            "item/completed" => item(params).and_then(completed).into_iter().collect(),
            "serverRequest/resolved" => params
                .get("requestId")
                .map(|request| Heard::Withdrawn {
                    request: request.clone(),
                })
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
}

pub(crate) fn answer(request: &Value, allow: bool) -> Vec<Heard> {
    let decision = if allow { "accept" } else { "decline" };
    vec![Heard::Send(
        json!({ "id": request, "result": { "decision": decision } }),
    )]
}

fn asked(id: Value, method: &str, params: Value) -> Vec<Heard> {
    let tool = match method {
        "item/commandExecution/requestApproval" => "command",
        "item/fileChange/requestApproval" => "file_change",
        _ => {
            let error = json!({ "code": NO_METHOD, "message": UNSUPPORTED });
            return vec![Heard::Send(json!({ "id": id, "error": error }))];
        }
    };
    vec![Heard::Asked {
        request: id,
        tool: tool.to_owned(),
        input: params,
    }]
}

fn turn_field(value: &Value, field: &str) -> Option<String> {
    value.get("turn")?.get(field)?.as_str().map(str::to_owned)
}

fn item(params: &Value) -> Option<Item> {
    serde_json::from_value::<ItemNote>(params.clone())
        .ok()
        .map(|note| note.item)
}

fn started(item: Item) -> Option<Heard> {
    let (call, tool, input) = match item {
        Item::CommandExecution { id, command, .. } => {
            (id, "command", json!({ "command": command }))
        }
        Item::FileChange { id, changes, .. } => (id, "file_change", json!({ "changes": changes })),
        Item::AgentMessage { .. } | Item::Other => return None,
    };
    Some(Heard::Event(RunEvent::ToolCall {
        call,
        tool: tool.to_owned(),
        input,
    }))
}

fn completed(item: Item) -> Option<Heard> {
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
    Some(Heard::Event(event))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::{Outcome, code};

    use super::*;
    use crate::run;
    use crate::test_support::{
        self, Done, FakeRun, answer, asked, ended, ends_as_recorded, finished, is_started, next,
        started, text, tool_done, until,
    };

    const THREAD: &str = "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b";

    fn fake(recording: &str, resume: Option<&str>) -> (FakeRun, Done) {
        let resume = resume.map(|id| AgentSession::try_from(id.to_owned()).unwrap());
        let mut wire = Wire::new(resume);
        let opening = wire.opening(&App {
            name: "weacon-core".to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        });
        test_support::fake(run::Wire::Codex(wire), opening, recording)
    }

    fn failed() -> RunEvent {
        ended(Outcome::Failed {
            code: code!("agent.turn_failed"),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_thread_runs_a_turn_and_an_approval_goes_out_and_back() {
        let (mut run, done) = fake(include_str!("../../sessions/codex/approval.replay"), None);

        run.send("Run the parser tests").await.unwrap();
        let events = until(&mut run, asked).await;
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
            until(&mut run, finished).await,
            [
                tool_done("item-1", false),
                text("All 12 tests pass."),
                ended(Outcome::Completed),
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
        let events = until(&mut run, finished).await;
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
    async fn a_refused_turn_fails_and_a_declined_change_fails_its_tool() {
        let (mut run, done) = fake(include_str!("../../sessions/codex/decline.replay"), None);

        run.send("Tidy the readme").await.unwrap();
        assert_eq!(until(&mut run, finished).await, [started(THREAD), failed()]);
        run.send("Tidy the readme").await.unwrap();
        let events = until(&mut run, asked).await;
        assert!(matches!(
            events.first(),
            Some(RunEvent::ToolCall { call, tool, .. }) if call == "item-5" && tool == "file_change"
        ));
        run.answer(&answer("run-1", false)).await.unwrap();
        assert_eq!(
            until(&mut run, finished).await,
            [tool_done("item-5", true), failed()]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn without_a_thread_every_prompt_fails() {
        let (mut run, done) = fake(include_str!("../../sessions/codex/no_thread.replay"), None);

        run.send("Explain the build").await.unwrap();
        assert_eq!(until(&mut run, finished).await, [failed()]);
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
        assert_eq!(next(&mut run).await, Some(ended(Outcome::Interrupted)));
        assert_eq!(next(&mut run).await, Some(started(THREAD)));
        ends_as_recorded(run, done).await;
    }
}
