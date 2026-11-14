//! A made-up chat log that the fake server keeps for its client: a person's prompt and a member's
//! Claude run's long answer, then another prompt and a shared Codex run waiting on a permission
//! prompt. It answers the client's chat queries and commands, and commits the recording's events,
//! the Claude run's, as they play. A prompt of exactly [`LOSE`] makes it tell the client it lost events, and a
//! query about any other chat is refused.

use serde_json::Value;
use weacon_model::ChatRuns;
use weacon_protocol::{
    AgentKind, AgentOwner, AgentRef, ChatAction, ChatAnswer, ChatAnswerKind, ChatCommand,
    ChatEntry, ChatEvent, ChatId, ChatQuery, ChatQueryKind, ChatUpdate, Envelope, ErrorBody,
    MemberId, Message, Outcome, PermissionAnswer, Receipt, RequestId, RunEvent, RunId, RunState,
    RunStatus, RunUpdate, WorkspaceId, code,
};

mod events;

use events::{agent, run, said};

/// The prompt text that makes the fake drop its client's subscription.
const LOSE: &str = "/lose";

// At most this many events go in a snapshot or a window.
const PAGE: usize = 50;

/// The chat's first prompt, more than a snapshot's worth of events before its latest.
const FIRST: &str = "Merge the duplicate lines in the shopping list.";
// The steps of the turn that answered the first prompt.
const EARLIER: u64 = 60;

/// The seq of the made-up log's last event; the server's tests read it.
#[cfg(test)]
pub(crate) const SEEDED: u64 = 69;

/// One chat's log, and the envelope id of the client's subscription to it.
#[derive(Debug)]
pub(crate) struct FakeChat {
    workspace: WorkspaceId,
    chat: ChatId,
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
            entries: Vec::new(),
            subscription: None,
            stopped: Vec::new(),
        };
        let (mine, shared) = (run("r-1")?, run("r-2")?);
        let member = MemberId::try_from("m-1".to_owned())?;
        let claude = agent(AgentKind::Claude, AgentOwner::Member { member }, "laptop")?;
        fake.prompt("q-0", FIRST)?;
        fake.start(&mine, claude);
        for step in 1..=EARLIER {
            fake.commit(said(
                &mine,
                &format!("Step {step} of the first pass is done."),
            ));
        }
        fake.finish(&mine, Outcome::Completed);
        fake.prompt("q-1", "Now merge across units too.")?;
        fake.start(
            &shared,
            agent(AgentKind::Codex, AgentOwner::Shared, "team VPS")?,
        );
        fake.commit(said(
            &shared,
            "I'll run the unit tests before I change anything.",
        ));
        fake.commit(ChatEvent::Run {
            update: RunUpdate {
                run: shared.clone(),
                event: RunEvent::PermissionAsked {
                    run: shared.clone(),
                    prompt: 1,
                    tool: "Bash".to_owned(),
                    input: Value::Null,
                },
            },
        });
        fake.status(&shared, RunState::Waiting);
        Ok(fake)
    }

    /// What answers `envelope` when it is a query about this chat or a command to it.
    pub(crate) fn answer(&mut self, envelope: &Envelope) -> Vec<Envelope> {
        if let Message::ChatQuery(ChatQuery {
            workspace,
            chat,
            query,
        }) = &envelope.message
        {
            if !self.is(workspace, chat) {
                let refused = ErrorBody {
                    code: code!("chat.unknown"),
                    retryable: false,
                };
                return vec![Envelope {
                    id: envelope.id,
                    message: Message::Error(refused),
                }];
            }
            return self.query(envelope.id, query);
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
            return Vec::new();
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

    /// Commits a recorded event of a run; what tells the client of it, if it subscribed.
    pub(crate) fn played(&mut self, envelope: &Envelope) -> Vec<Envelope> {
        let Message::Run(update) = &envelope.message else {
            return Vec::new();
        };
        if !self.plays(envelope) {
            return Vec::new();
        }
        let seq = self.seq();
        self.play(update);
        self.told_since(seq)
    }

    /// Whether a recorded message is still played: not one of a run a person stopped.
    pub(crate) fn plays(&self, envelope: &Envelope) -> bool {
        !matches!(&envelope.message, Message::Run(update) if self.stopped.contains(&update.run))
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
            ChatAction::Prompt { text, agent, .. } => self.commit(ChatEvent::Prompt {
                request: request.clone(),
                text: text.clone(),
                repo: None,
                agent: *agent,
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
            ChatAction::Newer => {}
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
    fn play(&mut self, update: &RunUpdate) {
        let run = &update.run;
        let known = self.last(run).map(|status| status.state);
        let finished = matches!(update.event, RunEvent::Finished { .. });
        if known == Some(RunState::Idle) && !finished {
            self.status(run, RunState::Running);
        }
        // Deltas stream to the client as they are, and are never committed.
        if !matches!(update.event, RunEvent::TextDelta { .. }) {
            self.commit(ChatEvent::Run {
                update: update.clone(),
            });
        }
        if known.is_none() {
            return;
        }
        if finished {
            self.status(run, RunState::Idle);
        } else if matches!(update.event, RunEvent::PermissionAsked { .. }) {
            self.status(run, RunState::Waiting);
        }
    }

    fn prompt(&mut self, request: &str, text: &str) -> Result<(), String> {
        let request = RequestId::try_from(request.to_owned())?;
        self.commit(ChatEvent::Prompt {
            request,
            text: text.to_owned(),
            repo: None,
            agent: None,
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
    use super::*;

    fn fake() -> FakeChat {
        FakeChat::new().unwrap()
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
        })
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
    fn the_chat_starts_with_a_finished_turn_then_a_shared_run_waiting_on_a_prompt() {
        let mut fake = fake();
        let (seq, runs, waiting) = snapshot(&mut fake);
        let member = MemberId::try_from("m-1".to_owned()).unwrap();
        let claude = agent(AgentKind::Claude, AgentOwner::Member { member }, "laptop").unwrap();
        let codex = agent(AgentKind::Codex, AgentOwner::Shared, "team VPS").unwrap();
        let log: Vec<_> = fake
            .entries
            .iter()
            .map(|entry| format!("{} {}", entry.seq, kind(&entry.event)))
            .filter(|told| !told.ends_with("r-1 text"))
            .collect();
        let agents: Vec<_> = fake.statuses().map(|status| status.agent.clone()).collect();

        assert_eq!(
            log,
            [
                format!("1 prompt {FIRST}"),
                "2 r-1 Running".to_owned(),
                "63 r-1 finished".to_owned(),
                "64 r-1 Idle".to_owned(),
                "65 prompt Now merge across units too.".to_owned(),
                "66 r-2 Running".to_owned(),
                "67 r-2 text".to_owned(),
                "68 r-2 permission_asked".to_owned(),
                format!("{SEEDED} r-2 Waiting"),
            ]
        );
        assert_eq!(
            agents,
            [claude.clone(), claude, codex.clone(), codex.clone()].map(Some)
        );
        assert_eq!((seq, waiting), (SEEDED, vec![SEEDED - 1]));
        assert_eq!(
            runs,
            [RunStatus {
                run: run("r-2").unwrap(),
                state: RunState::Waiting,
                agent: Some(codex)
            }]
        );
    }

    #[test]
    fn a_subscription_gets_what_it_missed_then_each_new_event_under_its_id() {
        let mut fake = fake();
        let missed = fake.answer(&query(
            4,
            "c-1",
            ChatQueryKind::Subscribe { after: SEEDED - 2 },
        ));
        let delta = fake.played(&played(RunEvent::TextDelta {
            text: "Mer".to_owned(),
        }));
        let text = fake.played(&played(RunEvent::Text {
            text: "Merged.".to_owned(),
        }));

        assert_eq!(
            shape(&missed),
            [
                (4, "68 r-2 permission_asked".to_owned()),
                (4, "69 r-2 Waiting".to_owned())
            ]
        );
        assert_eq!(shape(&delta), [(4, "70 r-1 Running".to_owned())]);
        assert_eq!(shape(&text), [(4, "71 r-1 text".to_owned())]);
        assert_eq!(
            shape(&fake.answer(&query(
                6,
                "c-1",
                ChatQueryKind::Subscribe { after: SEEDED + 9 }
            ))),
            [(6, "lost".to_owned())]
        );
    }

    #[test]
    fn events_played_before_a_subscription_are_kept_for_it() {
        let mut fake = fake();
        assert!(
            fake.played(&played(RunEvent::Text {
                text: "Merged.".to_owned()
            }))
            .is_empty()
        );

        assert_eq!(snapshot(&mut fake).0, 71);
        assert_eq!(
            shape(&fake.answer(&query(2, "c-1", ChatQueryKind::Subscribe { after: SEEDED }))),
            [
                (2, "70 r-1 Running".to_owned()),
                (2, "71 r-1 text".to_owned())
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
        for _ in 0..60 {
            fake.played(&played(RunEvent::Text {
                text: "More.".to_owned(),
            }));
        }
        let Message::ChatAnswer(ChatAnswer {
            answer: ChatAnswerKind::Snapshot { events, .. },
            ..
        }) = &fake.answer(&query(1, "c-1", ChatQueryKind::Snapshot))[0].message
        else {
            panic!("no snapshot");
        };
        assert_eq!(events.len(), PAGE);
        assert_eq!(events.last().map(|entry| entry.seq), Some(130));
    }

    #[test]
    fn an_interrupt_finishes_the_waiting_run_which_then_plays_no_further() {
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let recorded = played_by(
            "r-2",
            RunEvent::Text {
                text: "Merged.".to_owned(),
            },
        );
        assert!(fake.plays(&recorded));
        let stopped = fake.answer(&command(ChatAction::Interrupt));

        assert_eq!(
            shape(&stopped),
            [
                (4, "70 receipt q-9".to_owned()),
                (4, "71 r-2 finished".to_owned()),
                (4, "72 r-2 Idle".to_owned())
            ]
        );
        assert_eq!(snapshot(&mut fake).2, Vec::<u64>::new());
        assert!(!fake.plays(&recorded));
        assert!(fake.played(&recorded).is_empty());
        assert!(fake.plays(&query(1, "c-1", ChatQueryKind::Snapshot)));
    }

    #[test]
    fn prompts_answers_and_interrupts_are_committed_and_told() {
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let said = fake.answer(&prompt("Also sort them."));
        let allowed = fake.answer(&command(ChatAction::Permission {
            answer: PermissionAnswer {
                run: run("r-2").unwrap(),
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
                (4, "73 r-2 permission_answered".to_owned()),
                (4, "74 r-2 Running".to_owned()),
                (4, "75 r-2 text".to_owned()),
                (4, "76 r-2 finished".to_owned()),
                (4, "77 r-2 Idle".to_owned())
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
        let answer = |id: &str, prompt| {
            command(ChatAction::Permission {
                answer: PermissionAnswer {
                    run: run(id).unwrap(),
                    prompt,
                    allow: true,
                },
            })
        };
        // While r-2's prompt 1 waits: the same prompt of another run, another prompt of r-2.
        let other_run = fake.answer(&answer("r-1", 1));
        let other_prompt = fake.answer(&answer("r-2", 2));
        fake.answer(&command(ChatAction::Interrupt));
        let late = fake.answer(&answer("r-2", 1));

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
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let lost = fake.answer(&prompt(LOSE));
        let after = fake.played(&played(RunEvent::Text {
            text: "Merged.".to_owned(),
        }));

        assert_eq!(
            shape(&lost),
            [
                (4, "70 receipt q-9".to_owned()),
                (4, format!("71 prompt {LOSE}")),
                (4, "lost".to_owned())
            ]
        );
        assert!(after.is_empty());
        assert!(fake.answer(&prompt(LOSE)).is_empty());
    }

    #[test]
    fn the_recorded_run_goes_idle_when_it_finishes_and_runs_again_with_its_next_event() {
        let mut fake = fake();
        fake.answer(&query(4, "c-1", ChatQueryKind::Subscribe { after: SEEDED }));
        let finished = fake.played(&played(RunEvent::Finished {
            outcome: Outcome::Completed,
        }));
        let delta = fake.played(&played(RunEvent::TextDelta {
            text: "Next".to_owned(),
        }));
        let again = fake.played(&played(RunEvent::Text {
            text: "Next pass.".to_owned(),
        }));
        let more = fake.played(&played(RunEvent::TextDelta {
            text: "More".to_owned(),
        }));

        assert_eq!(
            shape(&finished),
            [
                (4, "70 r-1 finished".to_owned()),
                (4, "71 r-1 Idle".to_owned())
            ]
        );
        // The next pass's first delta runs it again, so the client streams its text.
        assert_eq!(shape(&delta), [(4, "72 r-1 Running".to_owned())]);
        assert_eq!(shape(&again), [(4, "73 r-1 text".to_owned())]);
        assert!(more.is_empty());
        let stranger = |event| Envelope {
            id: 0,
            message: Message::Run(RunUpdate {
                run: run("r-9").unwrap(),
                event,
            }),
        };
        let unknown_delta = stranger(RunEvent::TextDelta {
            text: "x".to_owned(),
        });
        assert!(fake.played(&unknown_delta).is_empty());
        let unknown_finished = stranger(RunEvent::Finished {
            outcome: Outcome::Completed,
        });
        assert_eq!(
            shape(&fake.played(&unknown_finished)),
            [(4, "74 r-9 finished".to_owned())]
        );
    }

    #[test]
    fn a_replayed_permission_prompt_makes_its_run_wait() {
        let mut fake = fake();
        let asked = fake.played(&played(RunEvent::PermissionAsked {
            run: run("r-1").unwrap(),
            prompt: 7,
            tool: "Edit".to_owned(),
            input: Value::Null,
        }));
        let (seq, runs, waiting) = snapshot(&mut fake);

        assert_eq!(shape(&asked), []);
        assert_eq!(seq, SEEDED + 3);
        assert_eq!(waiting, [SEEDED + 2]);
        assert_eq!(
            runs.iter()
                .map(|status| (status.run.as_str(), status.state))
                .collect::<Vec<_>>(),
            [("r-1", RunState::Waiting)]
        );
    }

    #[test]
    fn another_chats_queries_are_refused_and_other_messages_get_nothing() {
        let mut fake = fake();

        assert_eq!(
            fake.answer(&query(1, "c-2", ChatQueryKind::Snapshot)),
            [Envelope {
                id: 1,
                message: Message::Error(ErrorBody {
                    code: code!("chat.unknown"),
                    retryable: false,
                }),
            }]
        );
        assert!(
            fake.answer(&played(RunEvent::Text {
                text: "x".to_owned()
            }))
            .is_empty()
        );
        assert!(
            fake.played(&query(1, "c-1", ChatQueryKind::Snapshot))
                .is_empty()
        );
        assert!(
            fake.answer(&query(1, "c-1", ChatQueryKind::Newer))
                .is_empty()
        );
    }
}
