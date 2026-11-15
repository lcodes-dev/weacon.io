//! The leave the daemon gives `weacon update` to install a build that restarts it: given only
//! while no chat is busy, it pauses new turns until the updater reports, hangs up, or the leave
//! runs out.

use tokio::sync::oneshot;
use weacon_protocol::{
    Code, Envelope, InstallAction, InstallCommand, Message, Outcome, Receipt, code,
};

use crate::chats::{LeaveId, LeaveStep, WeakChats};
use crate::session::Reply;

/// The leave one connection holds, if any; the engine hears it hung up once this is dropped.
#[derive(Debug, Default)]
pub(super) struct Leave(Option<(LeaveId, oneshot::Sender<()>)>);

impl Leave {
    pub(super) async fn answer(
        &mut self,
        chats: &WeakChats,
        id: u32,
        command: InstallCommand,
    ) -> Reply {
        let taken = match command.action {
            InstallAction::Leave => self.ask(chats).await,
            InstallAction::Restarting => self.step(chats, LeaveStep::Restarting).await,
            InstallAction::Failed => self.step(chats, LeaveStep::Failed).await,
            InstallAction::Newer => Err(code!("message.unsupported")),
        };
        match taken {
            Ok(()) => Reply {
                envelope: Some(Envelope {
                    id,
                    message: Message::Receipt(Receipt {
                        request: command.request,
                        outcome: Outcome::Completed,
                    }),
                }),
                close: false,
            },
            Err(code) => Reply::error(id, code, false),
        }
    }

    async fn ask(&mut self, chats: &WeakChats) -> Result<(), Code> {
        // An engine that is stopping or doesn't answer in time counts as busy.
        let Ok(Some(leave)) = chats.leave().await else {
            return Err(code!("update.chat_busy"));
        };
        let (held, hung_up) = oneshot::channel();
        let chats = chats.clone();
        tokio::spawn(async move {
            drop(hung_up.await);
            drop(chats.step(leave, LeaveStep::HungUp).await);
        });
        self.0 = Some((leave, held));
        Ok(())
    }

    // The engine takes `step` only of the leave in force, so none that ran out is acted on.
    async fn step(&self, chats: &WeakChats, step: LeaveStep) -> Result<(), Code> {
        let Some((leave, _)) = &self.0 else {
            return Err(code!("update.no_leave"));
        };
        match chats.step(*leave, step).await {
            Ok(true) => Ok(()),
            _ => Err(code!("update.no_leave")),
        }
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ChatAction, RequestId};

    use super::*;
    use crate::chats::LEAVE_LASTS;
    use crate::chats::test_support::{Bench, bench, client, command, prompt, soon};
    use crate::test_support::error_code;

    fn install(action: InstallAction) -> InstallCommand {
        InstallCommand {
            request: RequestId::try_from("i-1".to_owned()).unwrap(),
            action,
        }
    }

    // Whether the daemon took `action`; otherwise the error it gave.
    async fn said(leave: &mut Leave, bench: &Bench, action: InstallAction) -> Result<(), String> {
        let reply = leave
            .answer(&bench.chats.downgrade(), 7, install(action))
            .await;
        let envelope = reply.envelope.unwrap();
        assert_eq!(envelope.id, 7);
        if let Message::Receipt(receipt) = &envelope.message {
            assert_eq!(receipt.request.as_str(), "i-1");
            assert_eq!(receipt.outcome, Outcome::Completed);
            return Ok(());
        }
        Err(error_code(&envelope.message).unwrap().to_owned())
    }

    async fn holds(bench: &Bench) -> Result<(), String> {
        let (holder, _pushes) = client(1);
        let hold = command("h-1", "c-1", ChatAction::Hold);
        let taken = bench.chats.command(hold, &holder).await;
        let released = command("h-2", "c-1", ChatAction::Release);
        drop(bench.chats.command(released, &holder).await);
        taken.map_err(|fault| fault.code().as_str().to_owned())
    }

    // Leave given while idle; a prompt sent then doesn't start, and no chat can be held.
    async fn paused(bench: &mut Bench, leave: &mut Leave) {
        assert_eq!(said(leave, bench, InstallAction::Leave).await, Ok(()));
        bench.command(prompt("r-1", "Hi")).await.unwrap();
        assert!(soon(bench.started.recv()).await.is_none());
        assert_eq!(holds(bench).await.unwrap_err(), "chat.busy");
    }

    #[tokio::test(start_paused = true)]
    async fn leave_is_refused_while_a_turn_runs_or_an_agent_starts() {
        let mut running = bench();
        running.command(prompt("r-1", "Hi")).await.unwrap();
        soon(running.started.recv()).await.unwrap();
        let refused = said(&mut Leave::default(), &running, InstallAction::Leave).await;
        assert_eq!(refused.unwrap_err(), "update.chat_busy");

        let starting = bench();
        let _slow = starting.hold_next_start();
        starting.command(prompt("r-1", "Hi")).await.unwrap();
        let refused = said(&mut Leave::default(), &starting, InstallAction::Leave).await;
        assert_eq!(refused.unwrap_err(), "update.chat_busy");
    }

    #[tokio::test(start_paused = true)]
    async fn leave_is_refused_while_a_chat_is_held() {
        let holding = bench();
        let (holder, _pushes) = client(1);
        let hold = command("h-1", "c-1", ChatAction::Hold);
        holding.chats.command(hold, &holder).await.unwrap();
        let refused = said(&mut Leave::default(), &holding, InstallAction::Leave).await;
        assert_eq!(refused.unwrap_err(), "update.chat_busy");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_install_resumes_turns_at_once() {
        let mut idle = bench();
        let mut leave = Leave::default();
        paused(&mut idle, &mut leave).await;
        assert_eq!(said(&mut leave, &idle, InstallAction::Failed).await, Ok(()));
        assert!(soon(idle.started.recv()).await.is_some());
        let again = said(&mut leave, &idle, InstallAction::Failed).await;
        assert_eq!(again.unwrap_err(), "update.no_leave");
    }

    #[tokio::test(start_paused = true)]
    async fn a_restart_keeps_turns_paused_after_the_updater_hangs_up_until_it_runs_out() {
        let mut idle = bench();
        let mut leave = Leave::default();
        paused(&mut idle, &mut leave).await;
        let restarting = said(&mut leave, &idle, InstallAction::Restarting).await;
        assert_eq!(restarting, Ok(()));
        // As when the restart stops the daemon, which drops the updater's connection.
        drop(leave);
        assert!(soon(idle.started.recv()).await.is_none());

        tokio::time::sleep(LEAVE_LASTS + std::time::Duration::from_secs(1)).await;
        assert!(soon(idle.started.recv()).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn an_updater_that_hangs_up_without_a_report_resumes_turns() {
        let mut idle = bench();
        let mut leave = Leave::default();
        paused(&mut idle, &mut leave).await;
        drop(leave);
        assert!(soon(idle.started.recv()).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn leave_that_ran_out_resumes_turns_and_allows_no_restart() {
        let mut idle = bench();
        let mut leave = Leave::default();
        paused(&mut idle, &mut leave).await;
        tokio::time::sleep(LEAVE_LASTS + std::time::Duration::from_secs(1)).await;
        assert!(soon(idle.started.recv()).await.is_some());
        let late = said(&mut leave, &idle, InstallAction::Restarting).await;
        assert_eq!(late.unwrap_err(), "update.no_leave");
    }

    #[tokio::test(start_paused = true)]
    async fn a_report_without_leave_or_an_unknown_action_is_refused() {
        let idle = bench();
        let mut leave = Leave::default();
        let report = said(&mut leave, &idle, InstallAction::Restarting).await;
        assert_eq!(report.unwrap_err(), "update.no_leave");
        let newer = said(&mut leave, &idle, InstallAction::Newer).await;
        assert_eq!(newer.unwrap_err(), "message.unsupported");
    }

    #[tokio::test(start_paused = true)]
    async fn an_engine_that_stopped_or_doesnt_answer_gives_no_leave() {
        let (silent, _engine) = crate::chats::test_support::parts();
        let refused = said(&mut Leave::default(), &silent, InstallAction::Leave).await;
        assert_eq!(refused.unwrap_err(), "update.chat_busy");
        let (stopped, engine) = crate::chats::test_support::parts();
        drop(engine);
        let refused = said(&mut Leave::default(), &stopped, InstallAction::Leave).await;
        assert_eq!(refused.unwrap_err(), "update.chat_busy");
    }
}
