use std::collections::VecDeque;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;
use weacon_observability::Fault;
use weacon_protocol::{AgentSession, Outcome, PermissionAnswer, RunEvent, RunId, code};

use super::wire::{self, Block, Line, Request, System};
use crate::lines::Lines;

// Far above any real line; a longer one is skipped rather than held in memory.
const MAX_LINE: usize = 16 << 20;

/// A conversation with one running `claude`, over its stdin and stdout.
pub struct Run<W, R> {
    id: RunId,
    stdin: W,
    stdout: Lines<R>,
    // Bytes not yet written; kept across a cancelled write so no line is cut.
    outbox: Vec<u8>,
    events: VecDeque<RunEvent>,
    prompts: Vec<Prompt>,
    next_prompt: u32,
    next_request: u32,
    turn: Turn,
    session: Option<AgentSession>,
    _child: Option<Child>,
}

struct Prompt {
    id: u32,
    request_id: String,
    input: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Turn {
    Idle,
    Running,
    Interrupting,
}

impl<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin> Run<W, R> {
    /// A run over an agent's pipes; `child`, when given, is killed when the run is dropped.
    pub(crate) fn new(id: RunId, stdin: W, stdout: R, child: Option<Child>) -> Self {
        Self {
            id,
            stdin,
            stdout: Lines::new(stdout, MAX_LINE),
            outbox: Vec::new(),
            events: VecDeque::new(),
            prompts: Vec::new(),
            next_prompt: 0,
            next_request: 0,
            turn: Turn::Idle,
            session: None,
            _child: child,
        }
    }

    /// Starts a turn with the person's `prompt`.
    ///
    /// # Errors
    /// When a turn is already running, or the agent's stdin is closed.
    pub async fn send(&mut self, prompt: &str) -> Result<(), Fault> {
        if self.turn != Turn::Idle {
            return Err(Fault::new(
                code!("agent.busy"),
                "The agent is still on a turn",
            ));
        }
        self.turn = Turn::Running;
        self.write(&wire::user(prompt)).await
    }

    /// Sends a person's answer to a permission prompt.
    ///
    /// # Errors
    /// When the prompt isn't waiting, or the agent's stdin is closed.
    pub async fn answer(&mut self, answer: &PermissionAnswer) -> Result<(), Fault> {
        let at = self
            .prompts
            .iter()
            .position(|prompt| prompt.id == answer.prompt)
            .filter(|_| answer.run == self.id)
            .ok_or_else(|| {
                Fault::new(
                    code!("agent.no_such_prompt"),
                    "That permission prompt isn't waiting",
                )
            })?;
        let prompt = self.prompts.swap_remove(at);
        self.write(&wire::permission(
            &prompt.request_id,
            answer.allow,
            &prompt.input,
        ))
        .await
    }

    /// Asks the agent to stop the current turn, which then finishes `interrupted`.
    ///
    /// # Errors
    /// When the agent's stdin is closed.
    pub async fn interrupt(&mut self) -> Result<(), Fault> {
        if self.turn != Turn::Running {
            return Ok(());
        }
        self.turn = Turn::Interrupting;
        self.next_request += 1;
        let request_id = format!("weacon-{}", self.next_request);
        self.write(&wire::interrupt(&request_id)).await
    }

    /// The next event, or `None` once the agent has exited. Safe to cancel.
    ///
    /// # Errors
    /// When the agent's output can't be read.
    pub async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
        loop {
            // A closed stdin means the agent is exiting, which its stdout then shows.
            if self.flush().await.is_err() {
                self.outbox.clear();
            }
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }
            let Some(line) = self.stdout.next().await.map_err(io_failed)? else {
                return Ok(self.exited());
            };
            if let Ok(line) = serde_json::from_slice(&line) {
                self.read(line);
            }
        }
    }

    fn read(&mut self, line: Line) {
        match line {
            Line::System(System::Init { session_id }) => self.started(session_id),
            Line::Assistant { message } | Line::User { message } => {
                for block in message.blocks() {
                    self.said(block);
                }
            }
            Line::Result { subtype, is_error } => {
                let outcome = match (subtype == "success" && !is_error, self.turn) {
                    (true, _) => Outcome::Completed,
                    (false, Turn::Interrupting) => Outcome::Interrupted,
                    (false, Turn::Idle | Turn::Running) => Outcome::Failed {
                        code: code!("agent.turn_failed"),
                    },
                };
                self.finished(outcome);
            }
            Line::ControlRequest {
                request_id,
                request: Request::CanUseTool { tool_name, input },
            } => self.asked(request_id, tool_name, input),
            Line::ControlRequest { request_id, .. } => {
                self.queue(&wire::unsupported(&request_id));
            }
            Line::ControlCancelRequest { request_id } => self.withdrawn(&request_id),
            Line::System(System::Other) | Line::Other => {}
        }
    }

    fn started(&mut self, session: AgentSession) {
        if self.session.as_ref() != Some(&session) {
            self.session = Some(session.clone());
            self.events.push_back(RunEvent::Started { session });
        }
    }

    fn said(&mut self, block: Block) {
        let event = match block {
            Block::Text { text } => RunEvent::Text { text },
            Block::ToolUse { id, name, input } => RunEvent::ToolCall {
                call: id,
                tool: name,
                input,
            },
            Block::ToolResult {
                tool_use_id,
                is_error,
            } => RunEvent::ToolDone {
                call: tool_use_id,
                failed: is_error,
            },
            Block::Other => return,
        };
        self.events.push_back(event);
    }

    fn asked(&mut self, request_id: String, tool: String, input: Value) {
        self.next_prompt += 1;
        let id = self.next_prompt;
        self.events.push_back(RunEvent::PermissionAsked {
            run: self.id.clone(),
            prompt: id,
            tool,
            input: input.clone(),
        });
        self.prompts.push(Prompt {
            id,
            request_id,
            input,
        });
    }

    fn withdrawn(&mut self, request_id: &str) {
        if let Some(at) = self
            .prompts
            .iter()
            .position(|prompt| prompt.request_id == request_id)
        {
            let prompt = self.prompts.swap_remove(at);
            self.events.push_back(RunEvent::PermissionWithdrawn {
                run: self.id.clone(),
                prompt: prompt.id,
            });
        }
    }

    fn finished(&mut self, outcome: Outcome) {
        self.turn = Turn::Idle;
        self.prompts.clear();
        self.events.push_back(RunEvent::Finished { outcome });
    }

    fn exited(&mut self) -> Option<RunEvent> {
        if self.turn == Turn::Idle {
            return None;
        }
        self.finished(Outcome::Unknown);
        self.events.pop_front()
    }

    async fn write(&mut self, line: &Value) -> Result<(), Fault> {
        self.queue(line);
        self.flush().await
    }

    fn queue(&mut self, line: &Value) {
        self.outbox.extend(line.to_string().as_bytes());
        self.outbox.push(b'\n');
    }

    async fn flush(&mut self) -> Result<(), Fault> {
        while !self.outbox.is_empty() {
            let written = self.stdin.write(&self.outbox).await.map_err(io_failed)?;
            if written == 0 {
                return Err(io_failed(std::io::ErrorKind::WriteZero.into()));
            }
            self.outbox.drain(..written);
        }
        self.stdin.flush().await.map_err(io_failed)
    }
}

fn io_failed(error: std::io::Error) -> Fault {
    Fault::new(code!("agent.io_failed"), "The agent's pipes failed").because(error)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;
    use tokio::io::{BufReader, DuplexStream};
    use tokio::task::JoinHandle;
    use tokio::time::timeout;
    use weacon_testing::replay;

    use super::*;

    type FakeRun = Run<DuplexStream, BufReader<DuplexStream>>;

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn fake(recording: &str) -> (FakeRun, JoinHandle<Result<(), String>>) {
        let replay = replay(recording);
        (
            Run::new(run_id("run-1"), replay.stdin, replay.stdout, None),
            replay.done,
        )
    }

    // Under paused time, a driver and fake stuck waiting on each other time out at once.
    async fn next(run: &mut FakeRun) -> Option<RunEvent> {
        timeout(Duration::from_secs(5), run.next())
            .await
            .unwrap()
            .unwrap()
    }

    // Events up to and including the first that `last` picks, or until the agent exits.
    async fn until(run: &mut FakeRun, last: fn(&RunEvent) -> bool) -> Vec<RunEvent> {
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

    fn asked(event: &RunEvent) -> bool {
        matches!(event, RunEvent::PermissionAsked { .. })
    }

    fn finished(event: &RunEvent) -> bool {
        matches!(event, RunEvent::Finished { .. })
    }

    fn run_id(id: &str) -> RunId {
        RunId::try_from(id.to_owned()).unwrap()
    }

    fn answer(run: &str, allow: bool) -> PermissionAnswer {
        PermissionAnswer {
            run: run_id(run),
            prompt: 1,
            allow,
        }
    }

    fn started() -> RunEvent {
        RunEvent::Started {
            session: AgentSession::try_from(SESSION.to_owned()).unwrap(),
        }
    }

    fn text(text: &str) -> RunEvent {
        RunEvent::Text {
            text: text.to_owned(),
        }
    }

    fn ended(outcome: Outcome) -> RunEvent {
        RunEvent::Finished { outcome }
    }

    async fn ends_as_recorded(mut run: FakeRun, done: JoinHandle<Result<(), String>>) {
        assert_eq!(next(&mut run).await, None);
        drop(run);
        timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_streams_its_events_and_a_permission_goes_out_and_back() {
        let (mut run, done) = fake(include_str!("../../sessions/claude/turn.replay"));
        let command = json!({ "command": "cargo test -p parser" });

        run.send("Run the parser tests").await.unwrap();
        assert_eq!(
            until(&mut run, asked).await,
            [
                started(),
                text("I'll run them."),
                RunEvent::ToolCall {
                    call: "toolu_01".to_owned(),
                    tool: "Bash".to_owned(),
                    input: command.clone(),
                },
                RunEvent::PermissionAsked {
                    run: run_id("run-1"),
                    prompt: 1,
                    tool: "Bash".to_owned(),
                    input: command,
                },
            ]
        );
        let other = run.answer(&answer("run-0", true)).await.unwrap_err();
        assert_eq!(other.code().as_str(), "agent.no_such_prompt");
        run.answer(&answer("run-1", true)).await.unwrap();
        assert_eq!(
            until(&mut run, finished).await,
            [
                RunEvent::ToolDone {
                    call: "toolu_01".to_owned(),
                    failed: false,
                },
                text("All 12 tests pass."),
                ended(Outcome::Completed),
            ]
        );
        let again = run.answer(&answer("run-1", true)).await.unwrap_err();
        assert_eq!(again.code().as_str(), "agent.no_such_prompt");
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_denial_is_sent_back_and_unhandled_requests_are_refused() {
        let (mut run, done) = fake(include_str!("../../sessions/claude/deny.replay"));

        run.send("Tidy the readme").await.unwrap();
        let events = until(&mut run, asked).await;
        assert_eq!(events.first(), Some(&started()));
        assert_eq!(events.len(), 3);
        run.answer(&answer("run-1", false)).await.unwrap();
        assert_eq!(
            until(&mut run, finished).await,
            [
                RunEvent::ToolDone {
                    call: "toolu_02".to_owned(),
                    failed: true,
                },
                ended(Outcome::Completed),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_withdraws_waiting_prompts_and_ends_the_turn_interrupted() {
        let (mut run, done) = fake(include_str!("../../sessions/claude/interrupt.replay"));

        run.interrupt().await.unwrap();
        run.send("Rewrite the lexer").await.unwrap();
        assert_eq!(until(&mut run, asked).await.len(), 2);
        run.interrupt().await.unwrap();
        run.interrupt().await.unwrap();
        assert_eq!(
            until(&mut run, finished).await,
            [
                RunEvent::PermissionWithdrawn {
                    run: run_id("run-1"),
                    prompt: 1,
                },
                ended(Outcome::Interrupted),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_cut_off_ends_unknown_and_resuming_never_resends_it() {
        let (mut run, done) = fake(include_str!("../../sessions/claude/cut_off.replay"));

        run.send("Bump the version").await.unwrap();
        assert_eq!(
            until(&mut run, finished).await,
            [started(), text("Bumping it now."), ended(Outcome::Unknown)]
        );
        ends_as_recorded(run, done).await;

        let (mut resumed, done) = fake(include_str!("../../sessions/claude/resume.replay"));
        resumed.send("Did the bump land?").await.unwrap();
        assert_eq!(
            until(&mut resumed, finished).await,
            [
                started(),
                text("It didn't; the file is unchanged."),
                ended(Outcome::Completed),
            ]
        );
        ends_as_recorded(resumed, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_error_fails_the_turn() {
        let (mut run, done) = fake(include_str!("../../sessions/claude/failed.replay"));

        run.send("Explain the build").await.unwrap();
        assert_eq!(
            until(&mut run, finished).await,
            [
                started(),
                ended(Outcome::Failed {
                    code: code!("agent.turn_failed"),
                }),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_gone_mid_prompt_still_ends_its_turn_unknown() {
        let (stdin, agent) = tokio::io::duplex(64);
        drop(agent);
        let printed = format!(
            "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"{SESSION}\"}}\n\
             {{\"type\":\"control_request\",\"request_id\":\"req-9\",\
             \"request\":{{\"subtype\":\"hook_callback\"}}}}\n\
             {{\"type\":\"control_request\",\"request_id\":\"req-10\",\
             \"request\":{{\"subtype\":\"can_use_tool\",\"tool_name\":\"Bash\",\"input\":{{}}}}}}\n"
        );
        let mut run = Run::new(run_id("run-1"), stdin, printed.as_bytes(), None);

        let sent = run.send("Hello").await.unwrap_err();
        assert_eq!(sent.code().as_str(), "agent.io_failed");
        assert_eq!(
            run.send("Again").await.unwrap_err().code().as_str(),
            "agent.busy"
        );
        assert_eq!(run.next().await.unwrap(), Some(started()));
        assert!(run.next().await.unwrap().as_ref().is_some_and(asked));
        let lost = run.answer(&answer("run-1", false)).await.unwrap_err();
        assert_eq!(lost.code().as_str(), "agent.io_failed");
        assert_eq!(run.next().await.unwrap(), Some(ended(Outcome::Unknown)));
        assert_eq!(run.next().await.unwrap(), None);
    }
}
