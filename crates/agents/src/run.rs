use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::Stdio;

use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use weacon_observability::Fault;
use weacon_protocol::{AgentSession, Outcome, PermissionAnswer, RunEvent, RunId, code};

use crate::access::{Check, Decision, Rules};
use crate::lines::Lines;
use crate::spawn::SpawnPolicy;
use crate::{claude, codex};

/// Sent back for a request the driver doesn't handle.
pub(crate) const UNSUPPORTED: &str = "Weacon doesn't handle this request.";

// Far above any real line; a longer one is skipped rather than held in memory.
const MAX_LINE: usize = 16 << 20;

/// A run of a real agent process.
pub type AgentRun = Run<ChildStdin, BufReader<ChildStdout>>;

/// A conversation with one running agent, over its stdin and stdout.
pub struct Run<W, R> {
    id: RunId,
    wire: Wire,
    stdin: W,
    stdout: Lines<R>,
    // Bytes not yet written; kept across a cancelled write so no line is cut.
    outbox: Vec<u8>,
    events: VecDeque<RunEvent>,
    prompts: Vec<Prompt>,
    next_prompt: u32,
    turn: Turn,
    session: Option<AgentSession>,
    rules: Rules,
    // Prompts about files waiting for their path to be checked, oldest first.
    checks: VecDeque<Check>,
    // Held only so the agent is killed when the run is dropped.
    child: Option<Child>,
}

/// Each agent's own way of saying things.
pub(crate) enum Wire {
    Claude(claude::Wire),
    Codex(codex::Wire),
}

/// What an agent's wire made of a line or a request, for the run to act on.
pub(crate) enum Heard {
    Send(Value),
    Started(AgentSession),
    Event(RunEvent),
    Asked {
        request: Value,
        tool: String,
        input: Value,
    },
    Withdrawn {
        request: Value,
    },
    Finished {
        ok: bool,
    },
}

impl Wire {
    fn opening(&mut self) -> Vec<Heard> {
        match self {
            Self::Claude(_) => Vec::new(),
            Self::Codex(wire) => wire.opening(),
        }
    }

    fn prompt(&mut self, text: &str) -> Vec<Heard> {
        match self {
            Self::Claude(_) => claude::prompt(text),
            Self::Codex(wire) => wire.prompt(text),
        }
    }

    fn answer(&self, request: &Value, allow: bool, input: &Value) -> Vec<Heard> {
        match self {
            Self::Claude(_) => claude::answer(request, allow, input),
            Self::Codex(_) => codex::answer(request, allow),
        }
    }

    fn interrupt(&mut self) -> Vec<Heard> {
        match self {
            Self::Claude(wire) => wire.interrupt(),
            Self::Codex(wire) => wire.interrupt(),
        }
    }

    // Codex's file changes always ask a person.
    fn changed_file(&self, tool: &str, input: &Value) -> Option<PathBuf> {
        match self {
            Self::Claude(_) => claude::changed_file(tool, input),
            Self::Codex(_) => None,
        }
    }

    fn read(&mut self, line: &[u8]) -> Vec<Heard> {
        match self {
            Self::Claude(_) => claude::read(line),
            Self::Codex(wire) => wire.read(line),
        }
    }
}

struct Prompt {
    id: u32,
    request: Value,
    input: Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Turn {
    Idle,
    Running,
    Interrupting,
}

/// Starts `command` under `policy` as run `id`, speaking `wire`, answering the prompts `rules`
/// decide.
pub(crate) fn spawn(
    mut command: Command,
    policy: &SpawnPolicy,
    id: RunId,
    wire: Wire,
    rules: Rules,
) -> Result<AgentRun, Fault> {
    let not_started = || Fault::new(code!("agent.not_started"), "The agent couldn't start");
    policy.apply(&mut command);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| not_started().because(error))?;
    policy.adjust(&child);
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(not_started());
    };
    let mut run = Run::new(id, wire, stdin, BufReader::new(stdout), rules);
    run.child = Some(child);
    Ok(run)
}

impl<W: AsyncWrite + Unpin, R: AsyncBufRead + Unpin> Run<W, R> {
    /// A run over an agent's pipes, answering the prompts `rules` decide. The wire's opening
    /// lines go out with the first write.
    pub(crate) fn new(id: RunId, mut wire: Wire, stdin: W, stdout: R, rules: Rules) -> Self {
        let opening = wire.opening();
        let mut run = Self {
            id,
            wire,
            stdin,
            stdout: Lines::new(stdout, MAX_LINE),
            outbox: Vec::new(),
            events: VecDeque::new(),
            prompts: Vec::new(),
            next_prompt: 0,
            turn: Turn::Idle,
            session: None,
            rules,
            checks: VecDeque::new(),
            child: None,
        };
        run.hear(opening);
        run
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
        let heard = self.wire.prompt(prompt);
        self.hear(heard);
        self.flush().await
    }

    /// Sends a person's answer to a permission prompt. Answers reach a prompt by run id, so
    /// whoever starts runs must never give two the same id.
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
        let heard = self
            .wire
            .answer(&prompt.request, answer.allow, &prompt.input);
        self.hear(heard);
        self.flush().await
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
        let heard = self.wire.interrupt();
        self.hear(heard);
        self.flush().await
    }

    /// The next event, or `None` once the agent has exited. Safe to cancel.
    ///
    /// # Errors
    /// When the agent's output can't be read.
    pub async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
        loop {
            self.settle_checks().await;
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
            let heard = self.wire.read(&line);
            self.hear(heard);
        }
    }

    fn hear(&mut self, heard: Vec<Heard>) {
        for each in heard {
            match each {
                Heard::Send(line) => {
                    self.outbox.extend(line.to_string().as_bytes());
                    self.outbox.push(b'\n');
                }
                Heard::Started(session) => self.started(session),
                Heard::Event(event) => self.events.push_back(event),
                Heard::Asked {
                    request,
                    tool,
                    input,
                } => self.asked(request, tool, input),
                Heard::Withdrawn { request } => self.withdrawn(&request),
                Heard::Finished { ok } => self.finished(self.outcome(ok)),
            }
        }
    }

    fn started(&mut self, session: AgentSession) {
        if self.session.as_ref() != Some(&session) {
            self.session = Some(session.clone());
            self.events.push_back(RunEvent::Started { session });
        }
    }

    fn asked(&mut self, request: Value, tool: String, input: Value) {
        match self.rules.decide(self.wire.changed_file(&tool, &input)) {
            Decision::Ask => self.ask(request, tool, input),
            Decision::Refuse => {
                let heard = self.wire.answer(&request, false, &input);
                self.hear(heard);
            }
            Decision::Check(file) => self.checks.push_back(Check {
                request,
                tool,
                input,
                file,
            }),
        }
    }

    // Safe to cancel: each check stays queued until its answer is in.
    async fn settle_checks(&mut self) {
        while let Some(check) = self.checks.front() {
            let (rules, file) = (self.rules.clone(), check.file.clone());
            let allowed = tokio::task::spawn_blocking(move || rules.may_change(&file))
                .await
                .unwrap_or(false);
            let Some(check) = self.checks.pop_front() else {
                return;
            };
            if allowed {
                let heard = self.wire.answer(&check.request, true, &check.input);
                self.hear(heard);
            } else {
                self.ask(check.request, check.tool, check.input);
            }
        }
    }

    fn ask(&mut self, request: Value, tool: String, input: Value) {
        self.next_prompt += 1;
        let id = self.next_prompt;
        self.events.push_back(RunEvent::PermissionAsked {
            run: self.id.clone(),
            prompt: id,
            tool,
            input: input.clone(),
        });
        self.prompts.push(Prompt { id, request, input });
    }

    fn withdrawn(&mut self, request: &Value) {
        if let Some(at) = self
            .prompts
            .iter()
            .position(|prompt| prompt.request == *request)
        {
            let prompt = self.prompts.swap_remove(at);
            self.events.push_back(RunEvent::PermissionWithdrawn {
                run: self.id.clone(),
                prompt: prompt.id,
            });
        }
    }

    const fn outcome(&self, ok: bool) -> Outcome {
        match (ok, self.turn) {
            (true, _) => Outcome::Completed,
            (false, Turn::Interrupting) => Outcome::Interrupted,
            (false, Turn::Idle | Turn::Running) => Outcome::Failed {
                code: code!("agent.turn_failed"),
            },
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
    use serde_json::json;
    use weacon_protocol::AccessLevel;

    use super::*;
    use crate::test_support::{
        self, Done, FakeRun, answer, ended, ends_as_recorded, is_asked, is_finished, run_id, text,
        tool_done, until,
    };

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    fn claude_wire() -> Wire {
        Wire::Claude(claude::Wire::default())
    }

    fn fake(recording: &str) -> (FakeRun, Done) {
        test_support::fake(claude_wire(), recording)
    }

    fn started() -> RunEvent {
        test_support::started(SESSION)
    }

    #[tokio::test(start_paused = true)]
    async fn a_turn_streams_its_events_and_a_permission_goes_out_and_back() {
        let (mut run, done) = fake(include_str!("../sessions/claude/turn.replay"));
        let command = json!({ "command": "cargo test -p parser" });

        run.send("Run the parser tests").await.unwrap();
        assert_eq!(
            until(&mut run, is_asked).await,
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
            until(&mut run, is_finished).await,
            [
                tool_done("toolu_01", false),
                text("All 12 tests pass."),
                ended(Outcome::Completed),
            ]
        );
        let again = run.answer(&answer("run-1", true)).await.unwrap_err();
        assert_eq!(again.code().as_str(), "agent.no_such_prompt");
        ends_as_recorded(run, done).await;
    }

    fn tool_call(call: &str, tool: &str, input: Value) -> RunEvent {
        RunEvent::ToolCall {
            call: call.to_owned(),
            tool: tool.to_owned(),
            input,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn read_only_access_turns_every_prompt_down_without_asking() {
        let (mut run, done) = fake(include_str!("../sessions/claude/deny.replay"));
        run.rules.level = AccessLevel::ReadOnly;
        let write = json!({ "file_path": "README.md", "content": "# Parser" });

        run.send("Tidy the readme").await.unwrap();

        assert_eq!(
            until(&mut run, is_finished).await,
            [
                started(),
                tool_call("toolu_02", "Write", write),
                tool_done("toolu_02", true),
                ended(Outcome::Completed),
            ]
        );
        ends_as_recorded(run, done).await;
    }

    fn full(cwd: &str) -> Rules {
        Rules {
            level: AccessLevel::Full,
            cwd: PathBuf::from(cwd),
            workspace: PathBuf::from("/missing"),
        }
    }

    // Real time: a path is checked on a blocking thread, which paused time doesn't wait for.
    // The recorded file is `/README.md`, in the working folder `/`, and the fake writes nothing.
    #[cfg(unix)]
    #[tokio::test]
    async fn full_access_lets_file_changes_inside_the_folder_run_and_asks_about_the_rest() {
        let (mut run, done) = fake(include_str!("../sessions/claude/full.replay"));
        run.rules = full("/");
        let write = json!({ "file_path": "/README.md", "content": "# Parser" });
        let command = json!({ "command": "cargo test" });

        run.send("Tidy the readme, then run the tests")
            .await
            .unwrap();
        assert_eq!(
            until(&mut run, is_asked).await,
            [
                started(),
                tool_call("toolu_04", "Write", write),
                tool_done("toolu_04", false),
                tool_call("toolu_05", "Bash", command.clone()),
                RunEvent::PermissionAsked {
                    run: run_id("run-1"),
                    prompt: 1,
                    tool: "Bash".to_owned(),
                    input: command,
                },
            ]
        );
        run.answer(&answer("run-1", true)).await.unwrap();

        assert_eq!(
            until(&mut run, is_finished).await,
            [tool_done("toolu_05", false), ended(Outcome::Completed)]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test]
    async fn full_access_asks_about_a_file_named_relative_to_where_the_agent_may_be() {
        let (mut run, done) = fake(include_str!("../sessions/claude/deny.replay"));
        run.rules = full("/");

        run.send("Tidy the readme").await.unwrap();
        let events = until(&mut run, is_asked).await;
        assert!(matches!(
            events.last(),
            Some(RunEvent::PermissionAsked { .. })
        ));
        run.answer(&answer("run-1", false)).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [tool_done("toolu_02", true), ended(Outcome::Completed)]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_denial_is_sent_back_and_unhandled_requests_are_refused() {
        let (mut run, done) = fake(include_str!("../sessions/claude/deny.replay"));

        run.send("Tidy the readme").await.unwrap();
        let events = until(&mut run, is_asked).await;
        assert_eq!(events.first(), Some(&started()));
        assert_eq!(events.len(), 3);
        run.answer(&answer("run-1", false)).await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [tool_done("toolu_02", true), ended(Outcome::Completed),]
        );
        ends_as_recorded(run, done).await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_interrupt_withdraws_waiting_prompts_and_ends_the_turn_interrupted() {
        let (mut run, done) = fake(include_str!("../sessions/claude/interrupt.replay"));

        run.interrupt().await.unwrap();
        run.send("Rewrite the lexer").await.unwrap();
        assert_eq!(until(&mut run, is_asked).await.len(), 2);
        run.interrupt().await.unwrap();
        run.interrupt().await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
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
        let (mut run, done) = fake(include_str!("../sessions/claude/cut_off.replay"));

        run.send("Bump the version").await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
            [started(), text("Bumping it now."), ended(Outcome::Unknown)]
        );
        ends_as_recorded(run, done).await;

        let (mut resumed, done) = fake(include_str!("../sessions/claude/resume.replay"));
        resumed.send("Did the bump land?").await.unwrap();
        assert_eq!(
            until(&mut resumed, is_finished).await,
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
        let (mut run, done) = fake(include_str!("../sessions/claude/failed.replay"));

        run.send("Explain the build").await.unwrap();
        assert_eq!(
            until(&mut run, is_finished).await,
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
        let mut run = Run::new(
            run_id("run-1"),
            claude_wire(),
            stdin,
            printed.as_bytes(),
            Rules::ask_first(),
        );

        let sent = run.send("Hello").await.unwrap_err();
        assert_eq!(sent.code().as_str(), "agent.io_failed");
        assert_eq!(
            run.send("Again").await.unwrap_err().code().as_str(),
            "agent.busy"
        );
        assert_eq!(run.next().await.unwrap(), Some(started()));
        assert!(run.next().await.unwrap().as_ref().is_some_and(is_asked));
        let lost = run.answer(&answer("run-1", false)).await.unwrap_err();
        assert_eq!(lost.code().as_str(), "agent.io_failed");
        assert_eq!(run.next().await.unwrap(), Some(ended(Outcome::Unknown)));
        assert_eq!(run.next().await.unwrap(), None);
    }

    #[test]
    fn the_refusal_the_agent_reads_keeps_its_size() {
        assert_eq!(UNSUPPORTED.len(), 35);
    }
}
