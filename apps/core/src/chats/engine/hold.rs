//! A chat held for a person who carries its session on by hand: no run starts meanwhile.

use weacon_observability::Fault;
use weacon_protocol::code;

use super::Engine;
use super::agent::Agent;
use super::rest::chat_fields;
use crate::chats::feed::Client;
use crate::runs::ChatKey;

impl<A: Agent> Engine<A> {
    /// Holds the chat while `holder`'s connection is open. Its idle agent stops, so its session
    /// is free, and prompts queue; a chat whose agent is on a turn isn't held.
    pub(super) async fn hold(&mut self, key: &ChatKey, holder: Client) -> Result<(), Fault> {
        if self.turn(key).is_some() || self.is_launching(key) {
            return Err(Fault::new(
                code!("chat.busy"),
                "The chat's agent is on a turn; carry on once it ends",
            ));
        }
        if self.is_live(key) {
            self.sleep(key).await?;
        }
        self.open(key).await?.held = Some(holder);
        Ok(())
    }

    /// Holds the chat for `holder`, or with none releases it.
    pub(super) async fn hold_or_release(
        &mut self,
        key: &ChatKey,
        holder: Option<Client>,
    ) -> Result<(), Fault> {
        match holder {
            Some(holder) => self.hold(key, holder).await,
            None => self.release(key).await,
        }
    }

    /// Ends the hold; prompts queued meanwhile go on.
    pub(super) async fn release(&mut self, key: &ChatKey) -> Result<(), Fault> {
        self.open(key).await?.held = None;
        self.advance(key).await
    }

    pub(super) fn is_held(&self, key: &ChatKey) -> bool {
        (self.open_chats.get(key))
            .and_then(|chat| chat.held.as_ref())
            .is_some_and(|holder| !holder.is_gone())
    }

    /// Lets go of each hold whose holder has gone without releasing it.
    pub(super) async fn release_gone(&mut self) {
        let gone: Vec<ChatKey> = (self.open_chats.iter())
            .filter(|(_, chat)| chat.held.as_ref().is_some_and(Client::is_gone))
            .map(|(key, _)| key.clone())
            .collect();
        for key in gone {
            if let Err(fault) = self.release(&key).await {
                self.log
                    .warn("chat not released", &chat_fields(&key, &fault));
            }
            self.settle(&key);
        }
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Outcome, RunState};

    use super::super::by_hand::{launched, on_turn};
    use crate::chats::test_support::{client, key, prompt, run, session, soon};

    #[tokio::test(start_paused = true)]
    async fn a_held_chat_queues_its_prompts_until_released_then_resumes_its_session() {
        let mut on = on_turn().await;
        let (holder, _pushes) = client(4);

        let refused = on.engine.hold(&key(), holder.clone()).await.unwrap_err();
        assert_eq!(refused.code().as_str(), "chat.busy");

        let run_1 = run("run-1");
        on.engine
            .end_turn(&key(), &run_1, Outcome::Completed)
            .await
            .unwrap();
        on.engine
            .runs
            .update(&key(), |record| record.session = Some(session()))
            .await
            .unwrap();
        on.engine.hold(&key(), holder).await.unwrap();
        assert_eq!(on.engine.runs.of(&key()).unwrap().state, RunState::Sleeping);
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        assert!(
            on.bench.started.try_recv().is_err(),
            "no run starts while it is held"
        );

        on.engine.release(&key()).await.unwrap();
        launched(&mut on.engine).await;
        let next = soon(on.bench.started.recv()).await.unwrap();
        assert_eq!(next.run_as.session, Some(session()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hold_whose_holder_has_gone_is_let_go_and_the_queue_goes_on() {
        let mut on = on_turn().await;
        let run_1 = run("run-1");
        on.engine
            .end_turn(&key(), &run_1, Outcome::Completed)
            .await
            .unwrap();
        let (holder, pushes) = client(4);
        on.engine.hold(&key(), holder).await.unwrap();
        on.engine
            .command(&key(), prompt("r-2", "More"))
            .await
            .unwrap();
        assert!(on.engine.is_held(&key()));

        drop(pushes);
        assert!(!on.engine.is_held(&key()));
        on.engine.release_gone().await;
        launched(&mut on.engine).await;
        assert!(soon(on.bench.started.recv()).await.is_some());
        assert!(
            on.engine
                .open_chats
                .get(&key())
                .is_some_and(|chat| chat.held.is_none())
        );
    }
}
