//! What people ask of a chat's agent: a prompt, a permission answer, an interrupt.

use weacon_observability::Fault;
use weacon_protocol::{
    AgentKind, ChatAction, ChatCommand, ChatEvent, Outcome, PermissionAnswer, RequestId, RunState,
    code,
};

use super::{Engine, Key, unsupported};
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
            // Whatever cut the first try short, a prompt it queued still gets its turn.
            Seen::Same => return self.advance(key).await,
            Seen::Other => {
                return Err(Fault::new(
                    code!("request.conflict"),
                    "That request id was used for another command",
                ));
            }
        }
        match command.action {
            ChatAction::Prompt { text } => {
                let committed = seen == Seen::Pending;
                self.prompt(key, request, text, committed).await
            }
            ChatAction::Permission { answer } => self.permission(key, request, answer).await,
            ChatAction::Interrupt => self.interrupt(key, request).await,
            ChatAction::Newer => Err(unsupported()),
        }
    }

    // A prompt is in the log before it is queued, and queued before it is accepted. Whichever
    // step a first try stopped at, sending it again takes it from there: `committed` says its
    // entry is in the log already.
    async fn prompt(
        &mut self,
        key: &Key,
        request: RequestId,
        text: String,
        committed: bool,
    ) -> Result<(), Fault> {
        if text.trim().is_empty() {
            return Err(Fault::new(
                code!("chat.empty_prompt"),
                "The prompt is empty",
            ));
        }
        if !committed {
            let prompt = ChatEvent::Prompt {
                request: request.clone(),
                text: text.clone(),
            };
            self.commit(key, prompt).await?;
        }
        let queued = Queued {
            request: request.clone(),
            text,
        };
        // Until a prompt can name its agent, a new chat runs Claude.
        let (agent, now) = (AgentKind::Claude, (self.host.now)());
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
        let asked = self.runs.is_latest(&key.0, &key.1, &answer.run);
        let waiting = &self.chat(key).await?.waiting;
        if !asked || !waiting.iter().any(|waits| waits.prompt == answer.prompt) {
            return Err(Fault::new(
                code!("agent.no_such_prompt"),
                "That permission prompt isn't waiting",
            ));
        }
        self.receipt(key, &request, Outcome::Completed).await?;
        let waiting = &mut self.chat(key).await?.waiting;
        waiting.retain(|waiting| waiting.prompt != answer.prompt);
        let (resumed, run) = (waiting.is_empty(), answer.run.clone());
        self.order(key, Order::Answer(answer));
        if resumed {
            self.state(key, &run, RunState::Running).await?;
        }
        Ok(())
    }

    async fn interrupt(&mut self, key: &Key, request: RequestId) -> Result<(), Fault> {
        if self.turn(key).is_some() {
            self.order(key, Order::Interrupt);
        }
        self.receipt(key, &request, Outcome::Completed).await
    }
}
