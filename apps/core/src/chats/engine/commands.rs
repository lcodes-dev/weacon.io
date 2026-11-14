//! What people ask of a chat's agent: a prompt, a permission answer, an interrupt.

use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, ChatAction, ChatCommand, ChatEvent, Outcome, PermissionAnswer, RequestId, RunState,
    code,
};

use super::{Engine, Key, Waiting, unsupported};
use crate::chats::log::Seen;
use crate::runs::Queued;
use crate::runs::task::{Agent, Order};

impl<A: Agent> Engine<A> {
    pub(super) async fn command(&mut self, key: &Key, command: ChatCommand) -> Result<(), Fault> {
        let request = command.request;
        let seen = self.chat(key).await?.log.seen(&request, &command.action);
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
            ChatAction::Prompt {
                text, repo, agent, ..
            } => {
                let runs = self.agent_of(key, agent)?;
                if seen != Seen::Pending {
                    let prompt = ChatEvent::Prompt {
                        request: request.clone(),
                        text: text.clone(),
                        repo,
                        agent,
                    };
                    self.log_prompt(key, prompt).await?;
                }
                self.prompt(key, request, text, runs).await
            }
            ChatAction::Permission { answer } => self.permission(key, request, answer).await,
            ChatAction::Interrupt => self.interrupt(key, request).await,
            ChatAction::Newer => Err(unsupported()),
        }
    }

    // The chat's agent: the one its first prompt named, or the core's default; a later prompt
    // can't change it.
    fn agent_of(&self, key: &Key, named: Option<AgentKind>) -> Result<AgentKind, Fault> {
        let chosen = self.runs.of(&key.0, &key.1).map(|record| record.agent);
        match (named, chosen) {
            (Some(AgentKind::Newer), _) => Err(unsupported()),
            (Some(named), Some(chosen)) if named != chosen => Err(Fault::new(
                code!("chat.agent_conflict"),
                "The chat already runs another agent",
            )),
            (named, chosen) => Ok(chosen.or(named).unwrap_or(AgentKind::Claude)),
        }
    }

    // A prompt is in the log before it is queued, and queued before it is accepted. Whichever
    // step a first try stopped at, sending it again takes it from there.
    async fn log_prompt(&mut self, key: &Key, prompt: ChatEvent) -> Result<(), Fault> {
        if let ChatEvent::Prompt { text, .. } = &prompt
            && text.trim().is_empty()
        {
            return Err(Fault::new(
                code!("chat.empty_prompt"),
                "The prompt is empty",
            ));
        }
        self.commit(key, prompt).await.map(drop)
    }

    async fn prompt(
        &mut self,
        key: &Key,
        request: RequestId,
        text: String,
        agent: AgentKind,
    ) -> Result<(), Fault> {
        let queued = Queued {
            request: request.clone(),
            text,
        };
        let now = (self.host.now)();
        self.runs
            .enqueue((&key.0, &key.1), agent, queued, now)
            .await?;
        self.receipt(key, &request, Outcome::Accepted).await?;
        self.advance(key).await
    }

    // The answer reaches only the chat's own run, and only for a prompt still waiting. Its
    // receipt is committed before the agent is told, so a refused answer was never given.
    async fn permission(
        &mut self,
        key: &Key,
        request: RequestId,
        answer: PermissionAnswer,
    ) -> Result<(), Fault> {
        if !self.waits_for(key, &answer).await? {
            return Err(Fault::new(
                code!("agent.no_such_prompt"),
                "That permission prompt isn't waiting",
            ));
        }
        let given = Some((request.clone(), answer.allow));
        let waiting = &mut self.chat(key).await?.waiting;
        for waits in waiting
            .iter_mut()
            .filter(|waits| waits.prompt == answer.prompt)
        {
            waits.answered.clone_from(&given);
        }
        self.receipt(key, &request, Outcome::Completed).await?;
        self.deliver(key, &request, answer).await
    }

    async fn waits_for(&mut self, key: &Key, answer: &PermissionAnswer) -> Result<bool, Fault> {
        let asked = self.runs.is_latest(&key.0, &key.1, &answer.run);
        let waiting = &self.chat(key).await?.waiting;
        Ok(asked && waiting.iter().any(|waits| waits.prompt == answer.prompt))
    }

    // Gives the answer `request` committed to its run, unless its prompt is no longer waiting.
    async fn deliver(
        &mut self,
        key: &Key,
        request: &RequestId,
        answer: PermissionAnswer,
    ) -> Result<(), Fault> {
        let given = Some((request.clone(), answer.allow));
        let committed = |waits: &Waiting| waits.prompt == answer.prompt && waits.answered == given;
        let waits = self.chat(key).await?.waiting.iter().any(committed);
        if !waits || !self.runs.is_latest(&key.0, &key.1, &answer.run) {
            return Ok(());
        }
        let waiting = &mut self.chat(key).await?.waiting;
        waiting.retain(|waiting| waiting.prompt != answer.prompt);
        let (resumed, run) = (waiting.is_empty(), answer.run.clone());
        self.order(key, Order::Answer(answer));
        if resumed {
            self.state(key, &run, RunState::Running).await?;
        }
        Ok(())
    }

    // Committed first and bound to the turn it stops, so a retry after a failed write still
    // stops that turn and never a later one.
    async fn interrupt(&mut self, key: &Key, request: RequestId) -> Result<(), Fault> {
        let stops = self.turn(key).map(|turn| (request.clone(), turn));
        self.chat(key).await?.interrupting = stops;
        self.receipt(key, &request, Outcome::Completed).await?;
        self.stop_turn(key, &request).await
    }

    // Sends the interrupt `request` committed, while the turn it stops is still running.
    async fn stop_turn(&mut self, key: &Key, request: &RequestId) -> Result<(), Fault> {
        let Some(turn) = self.turn(key) else {
            return Ok(());
        };
        let stops = Some((request.clone(), turn));
        let chat = self.chat(key).await?;
        if chat.interrupting != stops {
            return Ok(());
        }
        chat.interrupting = None;
        self.order(key, Order::Interrupt);
        Ok(())
    }
}
