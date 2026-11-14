//! What people ask of a chat's agent: a prompt, a permission answer, an interrupt.

use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, ChatAction, ChatCommand, ChatEvent, Outcome, PermissionAnswer, RepoId, RequestId,
    RunEvent, RunState, RunUpdate, code,
};

use super::{Answered, ChatKey, Engine, unsupported};
use crate::chats::log::Seen;
use crate::runs::Queued;
use crate::runs::task::{Agent, Order};

impl<A: Agent> Engine<A> {
    pub(super) async fn command(
        &mut self,
        key: &ChatKey,
        command: ChatCommand,
    ) -> Result<(), Fault> {
        let request = command.request;
        let seen = self.open(key).await?.log.seen(&request, &command.action);
        let seen = seen.await?;
        match seen {
            Seen::Never | Seen::Pending => {}
            // Whatever cut the first try short, what it began still happens: an answer still
            // waiting reaches its run, and a prompt it queued gets its turn.
            Seen::Same => {
                return match command.action {
                    ChatAction::Permission { answer } => self.deliver(key, &request, answer).await,
                    ChatAction::Interrupt => self.stop_turn(key, &request).await,
                    ChatAction::Prompt { .. } | ChatAction::Newer => self.advance(key).await,
                };
            }
            Seen::Other => {
                return Err(Fault::new(
                    code!("request.conflict"),
                    "That request id was used for another command",
                ));
            }
        }
        match command.action {
            ChatAction::Prompt { text, repo, .. } => {
                if seen != Seen::Pending {
                    self.log_prompt(key, &request, &text, repo).await?;
                }
                self.prompt(key, request, text).await
            }
            ChatAction::Permission { answer } => self.permission(key, request, answer).await,
            ChatAction::Interrupt => self.interrupt(key, request).await,
            ChatAction::Newer => Err(unsupported()),
        }
    }

    // A prompt is in the log before it is queued, and queued before it is accepted. Whichever
    // step a first try stopped at, sending it again takes it from there.
    async fn log_prompt(
        &mut self,
        key: &ChatKey,
        request: &RequestId,
        text: &str,
        repo: Option<RepoId>,
    ) -> Result<(), Fault> {
        if text.trim().is_empty() {
            return Err(Fault::new(
                code!("chat.empty_prompt"),
                "The prompt is empty",
            ));
        }
        let prompt = ChatEvent::Prompt {
            request: request.clone(),
            text: text.to_owned(),
            repo,
        };
        self.commit(key, prompt).await.map(drop)
    }

    async fn prompt(
        &mut self,
        key: &ChatKey,
        request: RequestId,
        text: String,
    ) -> Result<(), Fault> {
        let queued = Queued {
            request: request.clone(),
            text,
        };
        // Until a prompt can name its agent, a new chat runs Claude.
        let (agent, now) = (AgentKind::Claude, (self.host.now)());
        self.runs.enqueue(key, agent, queued, now).await?;
        self.commit_receipt(key, &request, Outcome::Accepted)
            .await?;
        self.advance(key).await
    }

    // The answer reaches only the chat's own run, and only for a prompt still waiting. Its
    // receipt is committed before the agent is told, so a refused answer was never given.
    async fn permission(
        &mut self,
        key: &ChatKey,
        request: RequestId,
        answer: PermissionAnswer,
    ) -> Result<(), Fault> {
        if !self.waits_for(key, &answer).await? {
            return Err(Fault::new(
                code!("agent.no_such_prompt"),
                "That permission prompt isn't waiting",
            ));
        }
        let answered = Answered {
            request: request.clone(),
            answer: answer.clone(),
        };
        let answers = &mut self.open(key).await?.answers;
        answers.retain(|other| other.answer.prompt != answer.prompt);
        answers.push(answered);
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
        self.deliver(key, &request, answer).await
    }

    // Whether the prompt waits, as the log says, for a person to answer it in a run still going.
    async fn waits_for(&mut self, key: &ChatKey, answer: &PermissionAnswer) -> Result<bool, Fault> {
        // The log forgets another run's prompts as soon as a newer run reports.
        let live = self.is_live(key);
        let log = &self.open(key).await?.log;
        Ok(live && log.runs().waits(&answer.run, answer.prompt))
    }

    // Gives the answer `request` committed to its run. It is logged unless the log has it
    // already, as after a write reported failed that still landed.
    async fn deliver(
        &mut self,
        key: &ChatKey,
        request: &RequestId,
        answer: PermissionAnswer,
    ) -> Result<(), Fault> {
        let given = Answered {
            request: request.clone(),
            answer: answer.clone(),
        };
        let live = self.is_live(key);
        let chat = self.open(key).await?;
        if !live || !chat.answers.contains(&given) {
            return Ok(());
        }
        if chat.log.runs().waits(&answer.run, answer.prompt) {
            // In the log before the agent is told, so a crash between the two leaves the truth.
            let update = RunUpdate {
                run: answer.run.clone(),
                event: RunEvent::PermissionAnswered {
                    answer: answer.clone(),
                },
            };
            self.commit(key, ChatEvent::Run { update }).await?;
        }
        let chat = self.open(key).await?;
        chat.answers.retain(|other| *other != given);
        let run = answer.run.clone();
        let resumed = !chat.log.runs().waiting.iter().any(|waits| waits.run == run);
        self.order(key, Order::Answer(answer));
        if resumed {
            self.set_state(key, &run, RunState::Running).await?;
        }
        Ok(())
    }

    // Committed first and bound to the turn it stops, so a retry after a failed write still
    // stops that turn and never a later one.
    async fn interrupt(&mut self, key: &ChatKey, request: RequestId) -> Result<(), Fault> {
        let stops = self.turn(key).map(|turn| (request.clone(), turn));
        self.open(key).await?.interrupting = stops;
        self.commit_receipt(key, &request, Outcome::Completed)
            .await?;
        self.stop_turn(key, &request).await
    }

    // Sends the interrupt `request` committed, while the turn it stops is still running.
    async fn stop_turn(&mut self, key: &ChatKey, request: &RequestId) -> Result<(), Fault> {
        let Some(turn) = self.turn(key) else {
            return Ok(());
        };
        let stops = Some((request.clone(), turn));
        let chat = self.open(key).await?;
        if chat.interrupting != stops {
            return Ok(());
        }
        chat.interrupting = None;
        self.order(key, Order::Interrupt);
        Ok(())
    }
}
