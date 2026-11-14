//! A made-up chat log that the fake server keeps for its client: a person's prompt and a long
//! answer, another prompt, a member's Claude run that plays the recording, and a shared Codex run
//! waiting on a permission prompt. It answers the client's chat queries and commands, and commits the recording's
//! events as they play. A prompt of exactly [`LOSE`] makes it tell the client it lost events.

use serde_json::Value;
use weacon_protocol::{
    AgentKind, AgentOwner, AgentRef, ChatAction, ChatAnswer, ChatAnswerKind, ChatCommand,
    ChatEntry, ChatEvent, ChatId, ChatQuery, ChatQueryKind, ChatUpdate, Envelope, MachineName,
    MemberId, Message, Outcome, RequestId, RunEvent, RunId, RunState, RunStatus, RunUpdate,
    WorkspaceId,
};

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
    runs: Vec<RunStatus>,
    subscription: Option<u32>,
}

impl FakeChat {
    /// The made-up chat `c-1` of workspace `w-1`.
    pub(crate) fn new() -> Result<Self, String> {
        let mut fake = Self {
            workspace: WorkspaceId::try_from("w-1".to_owned())?,
            chat: ChatId::try_from("c-1".to_owned())?,
            entries: Vec::new(),
            runs: Vec::new(),
            subscription: None,
        };
        let (mine, shared) = (run("r-1")?, run("r-2")?);
        let member = MemberId::try_from("m-1".to_owned())?;
        let claude = agent(AgentKind::Claude, AgentOwner::Member { member }, "laptop")?;
        fake.prompt("q-0", FIRST)?;
        fake.status(&mine, RunState::Running, claude.clone());
        for step in 1..=EARLIER {
            fake.commit(said(
                &mine,
                &format!("Step {step} of the first pass is done."),
            ));
        }
        fake.finish(&mine, Outcome::Completed, claude.clone());
        fake.prompt("q-1", "Now merge across units too.")?;
        fake.status(&mine, RunState::Running, claude);
        fake.status(
            &shared,
            RunState::Waiting,
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
                    run: shared,
                    prompt: 1,
                    tool: "Bash".to_owned(),
                    input: Value::Null,
                },
            },
        });
        Ok(fake)
    }

    /// What answers `envelope` when it is a query about this chat or a command to it.
    pub(crate) fn answer(&mut self, envelope: &Envelope) -> Vec<Envelope> {
        if let Message::ChatQuery(ChatQuery {
            workspace,
            chat,
            query,
        }) = &envelope.message
            && self.is(workspace, chat)
        {
            return self.query(envelope.id, query);
        }
        if let Message::ChatCommand(ChatCommand {
            workspace,
            chat,
            request,
            action,
            ..
        }) = &envelope.message
            && self.is(workspace, chat)
        {
            return self.command(request, action);
        }
        Vec::new()
    }

    /// Commits a recorded event of a run; what tells the client of it, if it subscribed.
    pub(crate) fn played(&mut self, envelope: &Envelope) -> Vec<Envelope> {
        let Message::Run(update) = &envelope.message else {
            return Vec::new();
        };
        if matches!(update.event, RunEvent::TextDelta { .. }) {
            return Vec::new();
        }
        self.commit(ChatEvent::Run {
            update: update.clone(),
        })
    }

    fn is(&self, workspace: &WorkspaceId, chat: &ChatId) -> bool {
        *workspace == self.workspace && *chat == self.chat
    }

    fn query(&mut self, id: u32, query: &ChatQueryKind) -> Vec<Envelope> {
        match query {
            ChatQueryKind::Snapshot => {
                let waiting = self.waiting();
                let events = self.before(self.seq() + 1, PAGE);
                vec![self.envelope(
                    id,
                    ChatAnswerKind::Snapshot {
                        seq: self.seq(),
                        runs: self.runs.clone(),
                        waiting,
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
                let missed = self
                    .entries
                    .iter()
                    .filter(|entry| entry.seq > *after)
                    .cloned();
                missed.map(|entry| self.update(id, entry)).collect()
            }
            ChatQueryKind::Subscribe { .. } => vec![self.envelope(id, ChatAnswerKind::EventsLost)],
            ChatQueryKind::Newer => Vec::new(),
        }
    }

    fn command(&mut self, request: &RequestId, action: &ChatAction) -> Vec<Envelope> {
        let mut told = Vec::new();
        match action {
            ChatAction::Prompt { text, .. } => {
                told.extend(self.commit(ChatEvent::Prompt {
                    request: request.clone(),
                    text: text.clone(),
                    repo: None,
                }));
                if text == LOSE
                    && let Some(id) = self.subscription.take()
                {
                    told.push(self.envelope(id, ChatAnswerKind::EventsLost));
                }
            }
            ChatAction::Permission { answer } => {
                let Some(agent) = self.agent_of(&answer.run) else {
                    return told;
                };
                let reply = if answer.allow {
                    "The tests pass."
                } else {
                    "I won't run them, then."
                };
                told.extend(self.status(&answer.run, RunState::Running, agent.clone()));
                told.extend(self.commit(said(&answer.run, reply)));
                told.extend(self.finish(&answer.run, Outcome::Completed, agent));
            }
            ChatAction::Interrupt => {
                let stopped: Vec<_> = self
                    .runs
                    .iter()
                    .filter(|status| matches!(status.state, RunState::Running | RunState::Waiting))
                    .filter_map(|status| Some((status.run.clone(), status.agent.clone()?)))
                    .collect();
                for (run, agent) in stopped {
                    told.extend(self.finish(&run, Outcome::Interrupted, agent));
                }
            }
            ChatAction::Newer => {}
        }
        told
    }

    fn prompt(&mut self, request: &str, text: &str) -> Result<Vec<Envelope>, String> {
        let request = RequestId::try_from(request.to_owned())?;
        Ok(self.commit(ChatEvent::Prompt {
            request,
            text: text.to_owned(),
            repo: None,
        }))
    }

    fn finish(&mut self, run: &RunId, outcome: Outcome, agent: AgentRef) -> Vec<Envelope> {
        let mut told = self.commit(ChatEvent::Run {
            update: RunUpdate {
                run: run.clone(),
                event: RunEvent::Finished { outcome },
            },
        });
        told.extend(self.status(run, RunState::Idle, agent));
        told
    }

    fn status(&mut self, run: &RunId, state: RunState, agent: AgentRef) -> Vec<Envelope> {
        let status = RunStatus {
            run: run.clone(),
            state,
            agent: Some(agent),
        };
        self.runs.retain(|known| known.run != *run);
        self.runs.push(status.clone());
        self.commit(ChatEvent::RunStatus { status })
    }

    fn commit(&mut self, event: ChatEvent) -> Vec<Envelope> {
        let entry = ChatEntry {
            seq: self.seq() + 1,
            event,
        };
        self.entries.push(entry.clone());
        self.subscription
            .map(|id| self.update(id, entry))
            .into_iter()
            .collect()
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

    // Where the permission prompts still waiting are: those of runs that wait, not yet answered.
    fn waiting(&self) -> Vec<u64> {
        let waits = |run: &RunId| {
            self.runs
                .iter()
                .any(|status| status.run == *run && status.state == RunState::Waiting)
        };
        self.entries
            .iter()
            .filter(|entry| asked(entry).is_some_and(waits))
            .map(|entry| entry.seq)
            .collect()
    }

    fn agent_of(&self, run: &RunId) -> Option<AgentRef> {
        self.runs
            .iter()
            .find(|status| status.run == *run)?
            .agent
            .clone()
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

// The run whose permission prompt `entry` is, if it is one.
fn asked(entry: &ChatEntry) -> Option<&RunId> {
    let ChatEvent::Run { update } = &entry.event else {
        return None;
    };
    matches!(update.event, RunEvent::PermissionAsked { .. }).then_some(&update.run)
}

fn run(id: &str) -> Result<RunId, String> {
    RunId::try_from(id.to_owned())
}

fn agent(kind: AgentKind, owner: AgentOwner, machine: &str) -> Result<AgentRef, String> {
    Ok(AgentRef {
        kind,
        owner,
        machine: Some(MachineName::try_from(machine.to_owned())?),
    })
}

fn said(run: &RunId, text: &str) -> ChatEvent {
    ChatEvent::Run {
        update: RunUpdate {
            run: run.clone(),
            event: RunEvent::Text {
                text: text.to_owned(),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::PermissionAnswer;

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
        })
    }

    fn played(event: RunEvent) -> Envelope {
        Envelope {
            id: 0,
            message: Message::Run(RunUpdate {
                run: run("r-1").unwrap(),
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
            ChatEvent::Receipt { .. } | ChatEvent::Newer => "other".to_owned(),
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
    fn the_chat_starts_with_a_prompt_two_agents_and_a_prompt_waiting() {
        let mut fake = fake();
        let (seq, runs, waiting) = snapshot(&mut fake);
        let first = fake.answer(&query(
            1,
            "c-1",
            ChatQueryKind::Window {
                before: 2,
                limit: 1,
            },
        ));

        assert_eq!((seq, waiting), (SEEDED, vec![SEEDED]));
        assert_eq!(shape(&first), [(1, "window [1]".to_owned())]);
        let agents: Vec<_> = runs
            .iter()
            .map(|status| {
                (
                    status.run.as_str(),
                    status.state,
                    status.agent.clone().unwrap(),
                )
            })
            .collect();
        assert_eq!(agents[0].0, "r-1");
        assert_eq!(agents[0].1, RunState::Running);
        assert_eq!(
            agents[0].2.owner,
            AgentOwner::Member {
                member: MemberId::try_from("m-1".to_owned()).unwrap()
            }
        );
        assert_eq!(agents[1].1, RunState::Waiting);
        assert_eq!(agents[1].2.owner, AgentOwner::Shared);
        assert_eq!(
            agents[1].2.machine.as_ref().map(MachineName::as_str),
            Some("team VPS")
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
                (4, "68 r-2 text".to_owned()),
                (4, "69 r-2 permission_asked".to_owned())
            ]
        );
        assert!(delta.is_empty());
        assert_eq!(shape(&text), [(4, "70 r-1 text".to_owned())]);
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

        assert_eq!(snapshot(&mut fake).0, 70);
        assert_eq!(
            shape(&fake.answer(&query(2, "c-1", ChatQueryKind::Subscribe { after: SEEDED }))),
            [(2, "70 r-1 text".to_owned())]
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
        assert_eq!(events.last().map(|entry| entry.seq), Some(129));
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

        assert_eq!(shape(&said), [(4, "70 prompt Also sort them.".to_owned())]);
        assert_eq!(
            shape(&allowed),
            [
                (4, "71 r-2 Running".to_owned()),
                (4, "72 r-2 text".to_owned()),
                (4, "73 r-2 finished".to_owned()),
                (4, "74 r-2 Idle".to_owned())
            ]
        );
        assert_eq!(
            shape(&stopped),
            [
                (4, "75 r-1 finished".to_owned()),
                (4, "76 r-1 Idle".to_owned())
            ]
        );
        assert!(unknown.is_empty());
        assert_eq!(snapshot(&mut fake).2, Vec::<u64>::new());
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
            [(4, format!("70 prompt {LOSE}")), (4, "lost".to_owned())]
        );
        assert!(after.is_empty());
        assert!(fake.answer(&prompt(LOSE)).is_empty());
    }

    #[test]
    fn another_chats_queries_and_other_messages_get_nothing() {
        let mut fake = fake();

        assert!(
            fake.answer(&query(1, "c-2", ChatQueryKind::Snapshot))
                .is_empty()
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
