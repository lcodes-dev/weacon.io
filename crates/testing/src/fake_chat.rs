//! A made-up chat log that the fake server keeps for its client: a person's prompt and a shared
//! Codex run's long answer, then another prompt and a member's Claude run. It commits the
//! recording's events as that run's, which asks to use Bash once the recording ends, and answers
//! the client's chat queries and commands. A prompt of exactly [`LOSE`] makes it tell the client
//! it lost events; a query or command about any other chat, or one it doesn't know, is refused.

use weacon_model::ChatRuns;
use weacon_protocol::{
    AgentKind, AgentOwner, AgentRef, ChatAction, ChatAnswer, ChatAnswerKind, ChatCommand,
    ChatEntry, ChatEvent, ChatId, ChatQuery, ChatQueryKind, ChatUpdate, Envelope, MemberId,
    Message, Outcome, PermissionAnswer, Receipt, RequestId, RunEvent, RunId, RunState, RunStatus,
    RunUpdate, WorkspaceId, code,
};

mod events;

use crate::fake_server::refusal;

use events::{agent, asks_bash, run, said, unknown};

/// The prompt text that makes the fake drop its client's subscription.
const LOSE: &str = "/lose";

// At most this many events go in a snapshot or a window.
const PAGE: usize = 50;

/// The chat's first prompt, more than a snapshot's worth of events before its latest.
const FIRST: &str = "Merge the duplicate lines in the shopping list.";
// The steps of the turn that answered the first prompt.
const EARLIER: u64 = 60;

/// The seq of the made-up log's last event once its run has asked to use Bash; the server's
/// tests read it.
#[cfg(test)]
pub(crate) const SEEDED: u64 = 69;

/// One chat's log, and the envelope id of the client's subscription to it.
#[derive(Debug)]
pub(crate) struct FakeChat {
    workspace: WorkspaceId,
    chat: ChatId,
    /// The client's own member, who writes every message and prompt.
    member: MemberId,
    entries: Vec<ChatEntry>,
    subscription: Option<u32>,
    /// Runs a person stopped, whose recording plays no further.
    stopped: Vec<RunId>,
}

impl FakeChat {
    /// The made-up chat `c-1` of workspace `w-1`.
    pub(crate) fn new() -> Result<Self, String> {
        let mut fake = Self {
            workspace: WorkspaceId::try_from("w-1".to_owned())?,
            chat: ChatId::try_from("c-1".to_owned())?,
            member: MemberId::try_from("m-1".to_owned())?,
            entries: Vec::new(),
            subscription: None,
            stopped: Vec::new(),
        };
        let (mine, shared) = (run("r-1")?, run("r-2")?);
        let member = fake.member.clone();
        fake.prompt("q-0", FIRST)?;
        fake.start(
            &shared,
            agent(AgentKind::Codex, AgentOwner::Shared, "team VPS")?,
        );
        for step in 1..=EARLIER {
            fake.commit(said(
                &shared,
                &format!("Step {step} of the first pass is done."),
            ));
        }
        fake.finish(&shared, Outcome::Completed);
        fake.prompt("q-1", "Now merge across units too.")?;
        fake.start(
            &mine,
            agent(AgentKind::Claude, AgentOwner::Member { member }, "laptop")?,
        );
        fake.commit(said(
            &mine,
            "I'll run the unit tests before I change anything.",
        ));
        Ok(fake)
    }

    /// What answers `envelope` when it is a query about this chat or a command to it.
    pub(crate) fn answer(&mut self, envelope: &Envelope) -> Vec<Envelope> {
        let id = envelope.id;
        let refused = |code| vec![refusal(id, code)];
        if unknown(&envelope.message) {
            return refused(code!("message.unsupported"));
        }
        if let Message::ChatQuery(ChatQuery {
            workspace,
            chat,
            query,
        }) = &envelope.message
        {
            if !self.is(workspace, chat) {
                return refused(code!("chat.not_found"));
            }
            return self.query(id, query);
        }
        let Message::ChatCommand(ChatCommand {
            workspace,
            chat,
            request,
            action,
            ..
        }) = &envelope.message
        else {
            return Vec::new();
        };
        if !self.is(workspace, chat) {
            return refused(code!("chat.not_found"));
        }
        let seq = self.seq();
        self.command(request, action);
        let mut told = self.told_since(seq);
        if matches!(action, ChatAction::Prompt { text, .. } if text == LOSE)
            && let Some(id) = self.subscription.take()
        {
            told.push(self.envelope(id, ChatAnswerKind::EventsLost));
        }
        told
    }

    /// Commits a recorded event of a run; what tells the client of it, if it subscribed. `None`
    /// when the message plays no further, as a run a person stopped.
    pub(crate) fn played(&mut self, envelope: &Envelope) -> Option<Vec<Envelope>> {
        let Message::Run(update) = &envelope.message else {
            return Some(Vec::new());
        };
        let current = self.current()?;
        // Only the chat's one run plays; a real run waiting on a permission is blocked and says
        // nothing, and a stopped one says nothing more.
        if current.run != update.run
            || current.state == RunState::Waiting
            || self.stopped.contains(&update.run)
        {
            return None;
        }
        let seq = self.seq();
        self.play(update, current.state);
        Some(self.told_since(seq))
    }

    /// Once the recording has played, its run asks to use Bash, unless a person stopped it or
    /// it has asked already; what tells the client of it.
    pub(crate) fn ended(&mut self) -> Vec<Envelope> {
        let Some(status) = self.current() else {
            return Vec::new();
        };
        let asked = self.entries.iter().any(|entry| {
            matches!(&entry.event, ChatEvent::Run { update } if update.run == status.run
                && matches!(update.event, RunEvent::PermissionAsked { .. }))
        });
        if asked || self.stopped.contains(&status.run) {
            return Vec::new();
        }
        let seq = self.seq();
        if status.state != RunState::Running {
            self.status(&status.run, RunState::Running);
        }
        self.ask(&status.run);
        self.told_since(seq)
    }

    fn is(&self, workspace: &WorkspaceId, chat: &ChatId) -> bool {
        *workspace == self.workspace && *chat == self.chat
    }

    fn query(&mut self, id: u32, query: &ChatQueryKind) -> Vec<Envelope> {
        match query {
            ChatQueryKind::Snapshot => {
                let events = self.before(self.seq() + 1, PAGE);
                vec![self.envelope(
                    id,
                    ChatAnswerKind::Snapshot {
                        seq: self.seq(),
                        runs: self.current().into_iter().collect(),
                        waiting: ChatRuns::fold(&self.entries).waiting_seqs(),
                        events,
                    },
                )]
            }
            ChatQueryKind::Window { before, limit } => {
                let limit = usize::try_from(*limit).unwrap_or(PAGE).min(PAGE);
                vec![self.envelope(
                    id,
                    ChatAnswerKind::Window {
                        events: self.before(*before, limit),
                    },
                )]
            }
            ChatQueryKind::Subscribe { after } if *after <= self.seq() => {
                self.subscription = Some(id);
                self.told_since(*after)
            }
            ChatQueryKind::Subscribe { .. } => vec![self.envelope(id, ChatAnswerKind::EventsLost)],
            ChatQueryKind::Newer => Vec::new(),
        }
    }

    // A command's receipt is an event in the log, as a core's is.
    fn command(&mut self, request: &RequestId, action: &ChatAction) {
        self.commit(ChatEvent::Receipt {
            receipt: Receipt {
                request: request.clone(),
                outcome: Outcome::Accepted,
            },
        });
        match action {
            ChatAction::Say { text, .. } => self.commit(ChatEvent::Message {
                author: self.member.clone(),
                text: text.clone(),
            }),
            ChatAction::Prompt { text, agent, .. } => self.commit(ChatEvent::Prompt {
                author: Some(self.member.clone()),
                request: request.clone(),
                text: text.clone(),
                repo: None,
                agent: *agent,
                task: None,
            }),
            ChatAction::Permission { answer } => self.answered(answer),
            ChatAction::Interrupt => {
                let working = self
                    .current()
                    .filter(|status| matches!(status.state, RunState::Running | RunState::Waiting));
                if let Some(RunStatus { run, .. }) = working {
                    self.finish(&run, Outcome::Interrupted);
                    self.stopped.push(run);
                }
            }
            // A hold comes only from a local client, never through the server.
            ChatAction::Hold | ChatAction::Release | ChatAction::Newer => {}
        }
    }

    // Commits the answer to a prompt still waiting, then plays the turn it lets the run finish.
    fn answered(&mut self, answer: &PermissionAnswer) {
        if !ChatRuns::fold(&self.entries).waits(&answer.run, answer.prompt) {
            return;
        }
        let reply = if answer.allow {
            "The tests pass."
        } else {
            "I won't run them, then."
        };
        self.commit(ChatEvent::Run {
            update: RunUpdate {
                run: answer.run.clone(),
                event: RunEvent::PermissionAnswered {
                    answer: answer.clone(),
                },
            },
        });
        self.status(&answer.run, RunState::Running);
        self.commit(said(&answer.run, reply));
        self.finish(&answer.run, Outcome::Completed);
    }

    // The recording's run goes idle at the end of each pass, and runs again with the next.
    fn play(&mut self, update: &RunUpdate, state: RunState) {
        let run = &update.run;
        let finished = matches!(update.event, RunEvent::Finished { .. });
        if state == RunState::Idle && !finished {
            self.status(run, RunState::Running);
        }
        // Deltas stream to the client as they are, and are never committed.
        if !matches!(update.event, RunEvent::TextDelta { .. }) {
            self.commit(ChatEvent::Run {
                update: update.clone(),
            });
        }
        if finished {
            self.status(run, RunState::Idle);
        } else if matches!(update.event, RunEvent::PermissionAsked { .. }) {
            self.status(run, RunState::Waiting);
        }
    }

    fn ask(&mut self, run: &RunId) {
        self.commit(asks_bash(run));
        self.status(run, RunState::Waiting);
    }

    fn prompt(&mut self, request: &str, text: &str) -> Result<(), String> {
        let request = RequestId::try_from(request.to_owned())?;
        self.commit(ChatEvent::Prompt {
            author: Some(self.member.clone()),
            request,
            text: text.to_owned(),
            repo: None,
            agent: None,
            task: None,
        });
        Ok(())
    }

    fn finish(&mut self, run: &RunId, outcome: Outcome) {
        self.commit(ChatEvent::Run {
            update: RunUpdate {
                run: run.clone(),
                event: RunEvent::Finished { outcome },
            },
        });
        self.status(run, RunState::Idle);
    }

    fn start(&mut self, run: &RunId, agent: AgentRef) {
        let status = RunStatus {
            run: run.clone(),
            state: RunState::Running,
            agent: Some(agent),
        };
        self.commit(ChatEvent::RunStatus { status });
    }

    // A run's state, under the agent its last status named.
    fn status(&mut self, run: &RunId, state: RunState) {
        let agent = self.last(run).and_then(|status| status.agent.clone());
        let status = RunStatus {
            run: run.clone(),
            state,
            agent,
        };
        self.commit(ChatEvent::RunStatus { status });
    }

    fn commit(&mut self, event: ChatEvent) {
        let entry = ChatEntry {
            seq: self.seq() + 1,
            event,
        };
        self.entries.push(entry);
    }

    // What tells the subscriber of the entries after `seq`.
    fn told_since(&self, seq: u64) -> Vec<Envelope> {
        let Some(id) = self.subscription else {
            return Vec::new();
        };
        let after = self.entries.iter().filter(|entry| entry.seq > seq);
        after.map(|entry| self.update(id, entry.clone())).collect()
    }

    fn seq(&self) -> u64 {
        self.entries.last().map_or(0, |entry| entry.seq)
    }

    // The latest `limit` events before `before`.
    fn before(&self, before: u64, limit: usize) -> Vec<ChatEntry> {
        let earlier: Vec<_> = self
            .entries
            .iter()
            .filter(|entry| entry.seq < before)
            .collect();
        let skip = earlier.len().saturating_sub(limit);
        earlier.into_iter().skip(skip).cloned().collect()
    }

    // The chat's current run, as its last status says.
    fn current(&self) -> Option<RunStatus> {
        self.statuses().next_back().cloned()
    }

    // The statuses in the log, oldest first.
    fn statuses(&self) -> impl DoubleEndedIterator<Item = &RunStatus> {
        self.entries.iter().filter_map(|entry| {
            let ChatEvent::RunStatus { status } = &entry.event else {
                return None;
            };
            Some(status)
        })
    }

    fn last(&self, run: &RunId) -> Option<&RunStatus> {
        self.statuses().rev().find(|status| status.run == *run)
    }

    fn update(&self, id: u32, entry: ChatEntry) -> Envelope {
        Envelope {
            id,
            message: Message::Chat(ChatUpdate {
                workspace: self.workspace.clone(),
                chat: self.chat.clone(),
                entry,
            }),
        }
    }

    fn envelope(&self, id: u32, answer: ChatAnswerKind) -> Envelope {
        Envelope {
            id,
            message: Message::ChatAnswer(ChatAnswer {
                workspace: self.workspace.clone(),
                chat: self.chat.clone(),
                answer,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    // The seeded chat once its recording has played: the member's run waits on its Bash prompt.
    fn fake() -> FakeChat {
        let mut fake = FakeChat::new().unwrap();
        fake.ended();
        fake
    }

    // The seq of the last event once the seeded prompt is allowed and its run is idle.
    const ANSWERED: u64 = SEEDED + 6;

    fn answered() -> FakeChat {
        let mut fake = fake();
        fake.answer(&allow("r-1", 1));
        fake
    }

    fn allow(id: &str, prompt: u32) -> Envelope {
        command(ChatAction::Permission {
            answer: PermissionAnswer {
                run: run(id).unwrap(),
                prompt,
                allow: true,
            },
        })
    }

    fn query(id: u32, chat: &str, query: ChatQueryKind) -> Envelope {
        Envelope {
            id,
            message: Message::ChatQuery(ChatQuery {
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from(chat.to_owned()).unwrap(),
                query,
            }),
        }
    }

    fn command(action: ChatAction) -> Envelope {
        Envelope {
            id: 9,
            message: Message::ChatCommand(ChatCommand {
                request: RequestId::try_from("q-9".to_owned()).unwrap(),
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                chat: ChatId::try_from("c-1".to_owned()).unwrap(),
                action,
                by: None,
            }),
        }
    }

    fn prompt(text: &str) -> Envelope {
        command(ChatAction::Prompt {
            text: text.to_owned(),
            repo: None,
            machine: None,
            agent: None,
            task: None,
        })
    }

    fn tell(fake: &mut FakeChat, envelope: &Envelope) -> Vec<Envelope> {
        fake.played(envelope).unwrap()
    }

    fn played(event: RunEvent) -> Envelope {
        played_by("r-1", event)
    }

    fn played_by(id: &str, event: RunEvent) -> Envelope {
        Envelope {
            id: 0,
            message: Message::Run(RunUpdate {
                run: run(id).unwrap(),
                event,
            }),
        }
    }

    // Each envelope's id and what it is: an answer's kind, or an update's seq and event kind.
    fn shape(envelopes: &[Envelope]) -> Vec<(u32, String)> {
        envelopes
            .iter()
            .map(|envelope| (envelope.id, told(&envelope.message)))
            .collect()
    }

    fn told(message: &Message) -> String {
        if let Message::Chat(update) = message {
            return format!("{} {}", update.entry.seq, kind(&update.entry.event));
        }
        let Message::ChatAnswer(answer) = message else {
            return format!("{message:?}");
        };
        match &answer.answer {
            ChatAnswerKind::EventsLost => "lost".to_owned(),
            ChatAnswerKind::Snapshot { seq, events, .. } => {
                format!("snapshot {seq} of {}", events.len())
            }
            ChatAnswerKind::Window { events } => {
                let seqs: Vec<_> = events.iter().map(|entry| entry.seq).collect();
                format!("window {seqs:?}")
            }
            ChatAnswerKind::Newer => "newer".to_owned(),
        }
    }

    fn kind(event: &ChatEvent) -> String {
        match event {
            ChatEvent::Prompt { text, .. } => format!("prompt {text}"),
            ChatEvent::OutsidePrompt { text } => format!("outside {text}"),
            ChatEvent::Run { update } => format!(
                "{} {}",
                update.run.as_str(),
                serde_json::to_value(&update.event).unwrap()["event"]
                    .as_str()
                    .unwrap()
            ),
            ChatEvent::RunStatus { status } => {
                format!("{} {:?}", status.run.as_str(), status.state)
            }
            ChatEvent::Receipt { receipt } => format!("receipt {}", receipt.request.as_str()),
            ChatEvent::Newer => "other".to_owned(),
        }
    }

    fn snapshot(fake: &mut FakeChat) -> (u64, Vec<RunStatus>, Vec<u64>) {
        let answer = fake.answer(&query(1, "c-1", ChatQueryKind::Snapshot));
        let Some(Message::ChatAnswer(ChatAnswer {
            answer: ChatAnswerKind::Snapshot {
                seq, runs, waiting, ..
            },
            ..
        })) = answer.into_iter().next().map(|envelope| envelope.message)
        else {
            panic!("no snapshot");
        };
        (seq, runs, waiting)
    }

    #[test]
    fn the_chat_starts_with_a_finished_turn_then_a_members_run_waiting_on_a_prompt() {
        let mut fake = fake();
        let (seq, runs, waiting) = snapshot(&mut fake);
        let member = MemberId::try_from("m-1".to_owned()).unwrap();
        let claude = agent(AgentKind::Claude, AgentOwner::Member { member }, "laptop").unwrap();
        let codex = agent(AgentKind::Codex, AgentOwner::Shared, "team VPS").unwrap();
        let log: Vec<_> = fake
            .entries
            .iter()
            .map(|entry| format!("{} {}", entry.seq, kind(&entry.event)))
            .filter(|told| !told.ends_with("r-2 text"))
            .collect();
        let agents: Vec<_> = fake.statuses().map(|status| status.agent.clone()).collect();

        assert_eq!(
            log,
            [
                format!("1 prompt {FIRST}"),
                "2 r-2 Running".to_owned(),
                "63 r-2 finished".to_owned(),
                "64 r-2 Idle".to_owned(),
                "65 prompt Now merge across units too.".to_owned(),
                "66 r-1 Running".to_owned(),
                "67 r-1 text".to_owned(),
                "68 r-1 permission_asked".to_owned(),
                format!("{SEEDED} r-1 Waiting"),
            ]
        );
        assert_eq!(
            agents,
            [codex.clone(), codex, claude.clone(), claude.clone()].map(Some)
        );
        assert_eq!((seq, waiting), (SEEDED, vec![SEEDED - 1]));
        assert_eq!(
            runs,
            [RunStatus {
                run: run("r-1").unwrap(),
                state: RunState::Waiting,
                agent: Some(claude)
            }]
        );
    }

    #[test]
    fn a_subscription_gets_what_it_missed_then_each_new_event_under_its_id() {
        let mut fake = answered();
        let missed = fake.answer(&query(
            4,
            "c-1",
            ChatQueryKind::Subscribe {
                after: ANSWERED - 2,
            },
        ));
        let delta = tell(
            &mut fake,
            &played(RunEvent::TextDelta {
                text: "Mer".to_owned(),
            }),
        );
        let text = tell(
            &mut fake,
            &played(RunEvent::Text {
                text: "Merged.".to_owned(),
            }),
        );

        assert_eq!(
            shape(&missed),
            [
                (4, "74 r-1 finished".to_owned()),
                (4, "75 r-1 Idle".to_owned())
            ]
        );
        assert_eq!(shape(&delta), [(4, "76 r-1 Running".to_owned())]);
        assert_eq!(shape(&text), [(4, "77 r-1 text".to_owned())]);
        assert_eq!(
            shape(&fake.answer(&query(
                6,
                "c-1",
                ChatQueryKind::Subscribe {
                    after: ANSWERED + 9
                }
            ))),
            [(6, "lost".to_owned())]
        );
    }

    #[test]
    fn events_played_before_a_subscription_are_kept_for_it() {
        let mut fake = answered();
        assert!(
            tell(
                &mut fake,
                &played(RunEvent::Text {
                    text: "Merged.".to_owned()
                })
            )
            .is_empty()
        );

        assert_eq!(snapshot(&mut fake).0, ANSWERED + 2);
        let after = ChatQueryKind::Subscribe { after: ANSWERED };
        assert_eq!(
            shape(&fake.answer(&query(2, "c-1", after))),
            [
                (2, "76 r-1 Running".to_owned()),
                (2, "77 r-1 text".to_owned())
            ]
        );
    }

    #[test]
    fn a_window_holds_the_latest_events_before_a_place() {
        let mut fake = fake();
        let window = |fake: &mut FakeChat, before, limit| {
            shape(&fake.answer(&query(3, "c-1", ChatQueryKind::Window { before, limit })))
        };

        assert_eq!(window(&mut fake, 4, 2), [(3, "window [2, 3]".to_owned())]);
        assert_eq!(window(&mut fake, 9, 1), [(3, "window [8]".to_owned())]);
        assert_eq!(window(&mut fake, 1, 9), [(3, "window []".to_owned())]);
        let Message::ChatAnswer(ChatAnswer {
            answer: ChatAnswerKind::Snapshot { events, .. },
            ..
        }) = &fake.answer(&query(1, "c-1", ChatQueryKind::Snapshot))[0].message
        else {
            panic!("no snapshot");
        };
        assert_eq!(events.len(), PAGE);
        assert_eq!(events.last().map(|entry| entry.seq), Some(SEEDED));
    }

    #[test]
    fn the_seeded_run_asks_to_use_bash_only_once_its_recording_has_ended() {
        let mut seeded = FakeChat::new().unwrap();
        let before = snapshot(&mut seeded);
        seeded.answer(&query(
            4,
            "c-1",
            ChatQueryKind::Subscribe { after: SEEDED - 2 },
        ));
        let asked = seeded.ended();

        assert_eq!(
            (before.0, before.1[0].state, before.2),
            (SEEDED - 2, RunState::Running, vec![])
        );
        assert_eq!(
            shape(&asked),
            [
                (4, "68 r-1 permission_asked".to_owned()),
                (4, "69 r-1 Waiting".to_owned())
            ]
        );
        assert_eq!(seeded.ended(), Vec::new());
        assert_eq!(snapshot(&mut seeded).2, vec![SEEDED - 1]);
    }

    #[test]
    fn an_idle_run_runs_again_to_ask_and_a_stopped_one_asks_nothing() {
        let mut idle = FakeChat::new().unwrap();
        idle.played(&played_by(
            "r-1",
            RunEvent::Finished {
                outcome: Outcome::Completed,
            },
        ));
        idle.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let asked = idle.ended();
        let mut stopped = FakeChat::new().unwrap();
        stopped.answer(&command(ChatAction::Interrupt));

        let (_, runs, waiting) = snapshot(&mut idle);
        assert_eq!((runs[0].state, waiting.len()), (RunState::Waiting, 1));
        assert_eq!(
            shape(&asked),
            [
                (4, "70 r-1 Running".to_owned()),
                (4, "71 r-1 permission_asked".to_owned()),
                (4, "72 r-1 Waiting".to_owned())
            ]
        );
        assert_eq!(stopped.ended(), Vec::new());
        assert_eq!(snapshot(&mut stopped).2, Vec::<u64>::new());
    }

    #[test]
    fn an_interrupt_finishes_the_waiting_run_which_then_plays_no_further() {
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let recorded = played_by(
            "r-1",
            RunEvent::Text {
                text: "Merged.".to_owned(),
            },
        );
        let stopped = fake.answer(&command(ChatAction::Interrupt));

        assert_eq!(
            shape(&stopped),
            [
                (4, "70 receipt q-9".to_owned()),
                (4, "71 r-1 finished".to_owned()),
                (4, "72 r-1 Idle".to_owned())
            ]
        );
        assert_eq!(snapshot(&mut fake).2, Vec::<u64>::new());
        assert_eq!(fake.played(&recorded), None);
        assert_eq!(
            fake.played(&query(1, "c-1", ChatQueryKind::Snapshot)),
            Some(Vec::new())
        );
    }

    #[test]
    fn prompts_answers_and_interrupts_are_committed_and_told() {
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let said = fake.answer(&prompt("Also sort them."));
        let allowed = fake.answer(&command(ChatAction::Permission {
            answer: PermissionAnswer {
                run: run("r-1").unwrap(),
                prompt: 1,
                allow: true,
            },
        }));
        let stopped = fake.answer(&command(ChatAction::Interrupt));
        let unknown = fake.answer(&command(ChatAction::Permission {
            answer: PermissionAnswer {
                run: run("r-9").unwrap(),
                prompt: 1,
                allow: true,
            },
        }));

        assert_eq!(
            shape(&said),
            [
                (4, "70 receipt q-9".to_owned()),
                (4, "71 prompt Also sort them.".to_owned())
            ]
        );
        assert_eq!(
            shape(&allowed),
            [
                (4, "72 receipt q-9".to_owned()),
                (4, "73 r-1 permission_answered".to_owned()),
                (4, "74 r-1 Running".to_owned()),
                (4, "75 r-1 text".to_owned()),
                (4, "76 r-1 finished".to_owned()),
                (4, "77 r-1 Idle".to_owned())
            ]
        );
        assert_eq!(shape(&stopped), [(4, "78 receipt q-9".to_owned())]);
        assert_eq!(shape(&unknown), [(4, "79 receipt q-9".to_owned())]);
        assert_eq!(snapshot(&mut fake).2, Vec::<u64>::new());
    }

    #[test]
    fn an_answer_to_a_prompt_not_waiting_is_received_and_does_nothing() {
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        // While r-1's prompt 1 waits: the same prompt of another run, another prompt of r-1.
        let other_run = fake.answer(&allow("r-2", 1));
        let other_prompt = fake.answer(&allow("r-1", 2));
        fake.answer(&command(ChatAction::Interrupt));
        let late = fake.answer(&allow("r-1", 1));

        assert_eq!(shape(&other_run), [(4, "70 receipt q-9".to_owned())]);
        assert_eq!(shape(&other_prompt), [(4, "71 receipt q-9".to_owned())]);
        assert_eq!(shape(&late), [(4, "75 receipt q-9".to_owned())]);
    }

    #[test]
    fn a_prompt_keeps_the_agent_it_names() {
        let mut fake = fake();
        let named = |agent| {
            command(ChatAction::Prompt {
                text: "Go.".to_owned(),
                repo: None,
                machine: None,
                agent,
                task: None,
            })
        };
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let told = [named(Some(AgentKind::Codex)), named(None)]
            .iter()
            .flat_map(|envelope| fake.answer(envelope))
            .collect::<Vec<_>>();

        let agents: Vec<_> = told
            .iter()
            .filter_map(|envelope| {
                if let Message::Chat(ChatUpdate { entry, .. }) = &envelope.message
                    && let ChatEvent::Prompt { agent, .. } = &entry.event
                {
                    return Some(*agent);
                }
                None
            })
            .collect();
        assert_eq!(agents, [Some(AgentKind::Codex), None]);
    }

    #[test]
    fn the_lose_prompt_ends_the_subscription_with_lost_events() {
        let mut fake = FakeChat::new().unwrap();
        fake.answer(&query(
            4,
            "c-1",
            ChatQueryKind::Subscribe { after: SEEDED - 2 },
        ));
        let lost = fake.answer(&prompt(LOSE));
        let after = tell(
            &mut fake,
            &played(RunEvent::Text {
                text: "Merged.".to_owned(),
            }),
        );

        assert_eq!(
            shape(&lost),
            [
                (4, "68 receipt q-9".to_owned()),
                (4, format!("69 prompt {LOSE}")),
                (4, "lost".to_owned())
            ]
        );
        assert!(after.is_empty());
        assert!(fake.answer(&prompt(LOSE)).is_empty());
    }

    #[test]
    fn the_recorded_run_goes_idle_when_it_finishes_and_runs_again_with_its_next_event() {
        let mut fake = answered();
        fake.answer(&query(
            4,
            "c-1",
            ChatQueryKind::Subscribe { after: ANSWERED },
        ));
        let finished = tell(
            &mut fake,
            &played(RunEvent::Finished {
                outcome: Outcome::Completed,
            }),
        );
        let delta = tell(
            &mut fake,
            &played(RunEvent::TextDelta {
                text: "Next".to_owned(),
            }),
        );
        let again = tell(
            &mut fake,
            &played(RunEvent::Text {
                text: "Next pass.".to_owned(),
            }),
        );
        let more = tell(
            &mut fake,
            &played(RunEvent::TextDelta {
                text: "More".to_owned(),
            }),
        );

        assert_eq!(
            shape(&finished),
            [
                (4, "76 r-1 finished".to_owned()),
                (4, "77 r-1 Idle".to_owned())
            ]
        );
        // The next pass's first delta runs it again, so the client streams its text.
        assert_eq!(shape(&delta), [(4, "78 r-1 Running".to_owned())]);
        assert_eq!(shape(&again), [(4, "79 r-1 text".to_owned())]);
        assert!(more.is_empty());
    }

    #[test]
    fn only_the_chats_current_run_plays() {
        let mut fake = FakeChat::new().unwrap();
        let other = |id| {
            played_by(
                id,
                RunEvent::Finished {
                    outcome: Outcome::Completed,
                },
            )
        };

        assert_eq!(fake.played(&other("r-9")), None);
        assert_eq!(fake.played(&other("r-2")), None);
        assert_eq!(snapshot(&mut fake).0, SEEDED - 2);
    }

    #[test]
    fn a_replayed_permission_prompt_makes_its_run_wait() {
        let mut fake = answered();
        let asked = tell(
            &mut fake,
            &played(RunEvent::PermissionAsked {
                run: run("r-1").unwrap(),
                prompt: 7,
                tool: "Edit".to_owned(),
                input: Value::Null,
            }),
        );
        let (seq, runs, waiting) = snapshot(&mut fake);

        assert_eq!(shape(&asked), []);
        assert_eq!(seq, ANSWERED + 3);
        assert_eq!(waiting, [ANSWERED + 2]);
        assert_eq!(
            runs.iter()
                .map(|status| (status.run.as_str(), status.state))
                .collect::<Vec<_>>(),
            [("r-1", RunState::Waiting)]
        );
    }

    #[test]
    fn a_waiting_runs_recorded_events_are_dropped_until_its_prompt_is_answered() {
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let recorded = [
            RunEvent::Text {
                text: "Merged.".to_owned(),
            },
            RunEvent::PermissionAsked {
                run: run("r-1").unwrap(),
                prompt: 2,
                tool: "Edit".to_owned(),
                input: Value::Null,
            },
            RunEvent::Finished {
                outcome: Outcome::Completed,
            },
        ];
        let dropped: Vec<_> = recorded
            .iter()
            .map(|event| fake.played(&played(event.clone())))
            .collect();

        assert_eq!(dropped, [None, None, None]);
        let (seq, _, waiting) = snapshot(&mut fake);
        assert_eq!((seq, waiting), (SEEDED, vec![SEEDED - 1]));
        assert_eq!(fake.answer(&allow("r-1", 1)).len(), 6);
        assert_eq!(
            shape(&tell(&mut fake, &played(recorded[0].clone()))),
            [
                (4, "76 r-1 Running".to_owned()),
                (4, "77 r-1 text".to_owned())
            ]
        );
    }

    #[test]
    fn another_chat_and_what_it_doesnt_know_are_refused_and_other_messages_get_nothing() {
        let mut fake = fake();
        let mut elsewhere = query(2, "c-1", ChatQueryKind::Snapshot);
        if let Message::ChatQuery(query) = &mut elsewhere.message {
            query.workspace = WorkspaceId::try_from("w-2".to_owned()).unwrap();
        }
        let mut commanded = command(ChatAction::Interrupt);
        if let Message::ChatCommand(command) = &mut commanded.message {
            command.workspace = WorkspaceId::try_from("w-2".to_owned()).unwrap();
        }
        let seq = fake.seq();

        assert_eq!(
            fake.answer(&query(1, "c-2", ChatQueryKind::Snapshot)),
            [refusal(1, code!("chat.not_found"))]
        );
        assert_eq!(
            fake.answer(&elsewhere),
            [refusal(2, code!("chat.not_found"))]
        );
        assert_eq!(
            fake.answer(&commanded),
            [refusal(commanded.id, code!("chat.not_found"))]
        );
        assert_eq!(
            fake.answer(&query(3, "c-1", ChatQueryKind::Newer)),
            [refusal(3, code!("message.unsupported"))]
        );
        let unnamed = command(ChatAction::Prompt {
            text: "Hi".to_owned(),
            repo: None,
            machine: None,
            agent: Some(AgentKind::Newer),
            task: None,
        });
        for newer in [command(ChatAction::Newer), unnamed] {
            assert_eq!(
                fake.answer(&newer),
                [refusal(newer.id, code!("message.unsupported"))]
            );
        }
        assert_eq!(fake.seq(), seq);
        assert!(
            fake.answer(&played(RunEvent::Text {
                text: "x".to_owned()
            }))
            .is_empty()
        );
    }
}
