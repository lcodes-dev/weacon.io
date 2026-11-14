//! The task that owns one running agent: it passes the engine's orders on and reports what
//! the agent says.

use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::time::Instant;
use weacon_agents::AgentRun;
use weacon_observability::Fault;
use weacon_protocol::{ChatId, PermissionAnswer, RunEvent, RunId, WorkspaceId, code};

/// What the engine needs of a running agent: the driver's run in the daemon, a fake in tests.
/// Dropping it stops the agent.
pub(crate) trait Agent: Send + 'static {
    fn send(&mut self, prompt: &str) -> impl Future<Output = Result<(), Fault>> + Send;
    fn answer(
        &mut self,
        answer: &PermissionAnswer,
    ) -> impl Future<Output = Result<(), Fault>> + Send;
    fn interrupt(&mut self) -> impl Future<Output = Result<(), Fault>> + Send;
    /// The next event, or `None` once the agent has exited. Safe to cancel.
    fn next(&mut self) -> impl Future<Output = Result<Option<RunEvent>, Fault>> + Send;
}

impl Agent for AgentRun {
    async fn send(&mut self, prompt: &str) -> Result<(), Fault> {
        Self::send(self, prompt).await
    }

    async fn answer(&mut self, answer: &PermissionAnswer) -> Result<(), Fault> {
        Self::answer(self, answer).await
    }

    async fn interrupt(&mut self) -> Result<(), Fault> {
        Self::interrupt(self).await
    }

    async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
        Self::next(self).await
    }
}

// Orders and reports carry prompts and agent output, so neither has a `Debug`.
pub(crate) enum Order {
    Send(String),
    Answer(PermissionAnswer),
    Interrupt,
}

/// Which run of which chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RunOf {
    pub(crate) workspace: WorkspaceId,
    pub(crate) chat: ChatId,
    pub(crate) run: RunId,
}

/// What a run's agent said, for the engine.
pub(crate) struct Report {
    pub(crate) of: RunOf,
    pub(crate) heard: Heard,
}

pub(crate) enum Heard {
    Event(RunEvent),
    /// The agent is gone, with why when its output couldn't be read; nothing follows.
    Exited(Option<Fault>),
}

// How long an agent whose pipes broke may still speak before it is stopped.
const LAST_WORDS: Duration = Duration::from_secs(5);
// How long an order may take to write; an agent that stops reading counts as broken.
const ORDER_WRITE: Duration = Duration::from_secs(30);

/// Drives `agent` as the run `of` until it exits or the engine drops `orders`.
pub(crate) async fn drive<A: Agent>(
    mut agent: A,
    of: RunOf,
    mut orders: mpsc::UnboundedReceiver<Order>,
    reports: mpsc::Sender<Report>,
) {
    // Once its pipes break, the agent takes no more orders and has a while to finish speaking.
    let mut broken: Option<(Fault, Instant)> = None;
    loop {
        let deadline = broken.as_ref().map_or_else(Instant::now, |(_, at)| *at);
        let heard = tokio::select! {
            order = orders.recv(), if broken.is_none() => {
                let Some(order) = order else { return };
                match obey(&mut agent, order).await {
                    // Closed, so the engine sends its next prompt to a new run.
                    Err(fault) if fault.code() == &code!("agent.io_failed") => {
                        orders.close();
                        broken = Some((fault, Instant::now() + LAST_WORDS));
                    }
                    // Any other refusal shows in what the agent says next.
                    Ok(()) | Err(_) => {}
                }
                continue;
            }
            () = tokio::time::sleep_until(deadline), if broken.is_some() => {
                Heard::Exited(broken.take().map(|(fault, _)| fault))
            }
            event = agent.next() => match event {
                Ok(Some(event)) => Heard::Event(event),
                Ok(None) => Heard::Exited(None),
                Err(fault) => Heard::Exited(Some(fault)),
            },
        };
        let exited = matches!(heard, Heard::Exited(_));
        let report = Report {
            of: of.clone(),
            heard,
        };
        if reports.send(report).await.is_err() || exited {
            return;
        }
    }
}

async fn obey<A: Agent>(agent: &mut A, order: Order) -> Result<(), Fault> {
    let obeyed = async {
        match order {
            Order::Send(prompt) => agent.send(&prompt).await,
            Order::Answer(answer) => agent.answer(&answer).await,
            Order::Interrupt => agent.interrupt().await,
        }
    };
    tokio::time::timeout(ORDER_WRITE, obeyed)
        .await
        .unwrap_or_else(|_| {
            Err(Fault::new(
                code!("agent.io_failed"),
                "The agent stopped reading",
            ))
        })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    // Says one text, then exits; it panics if asked for more after that.
    struct Brief {
        said: u8,
    }

    impl Agent for Brief {
        async fn send(&mut self, _prompt: &str) -> Result<(), Fault> {
            Ok(())
        }

        async fn answer(&mut self, _answer: &PermissionAnswer) -> Result<(), Fault> {
            Ok(())
        }

        async fn interrupt(&mut self) -> Result<(), Fault> {
            Ok(())
        }

        async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
            self.said += 1;
            match self.said {
                1 => Ok(Some(RunEvent::Text {
                    text: "Hello.".to_owned(),
                })),
                2 => Ok(None),
                _ => panic!("asked again after it exited"),
            }
        }
    }

    #[tokio::test]
    async fn a_run_reports_what_its_agent_says_then_its_exit_once_and_stops() {
        let of = RunOf {
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            chat: ChatId::try_from("c-1".to_owned()).unwrap(),
            run: RunId::try_from("run-1".to_owned()).unwrap(),
        };
        let (_orders, ordered) = mpsc::unbounded_channel();
        let (reporter, mut reports) = mpsc::channel(8);

        let driven = drive(Brief { said: 0 }, of.clone(), ordered, reporter);
        tokio::time::timeout(Duration::from_secs(5), driven)
            .await
            .unwrap();

        let first = reports.recv().await.unwrap();
        assert!(first.of == of && matches!(first.heard, Heard::Event(RunEvent::Text { .. })));
        let last = reports.recv().await.unwrap();
        assert!(matches!(last.heard, Heard::Exited(None)));
        assert!(reports.recv().await.is_none());
    }

    // Refuses every prompt: as busy, with broken pipes, or by never reading it, as the prompt
    // says. A second after it is asked, it says one text, then nothing more.
    struct Refuses {
        said: bool,
    }

    impl Agent for Refuses {
        async fn send(&mut self, prompt: &str) -> Result<(), Fault> {
            Err(match prompt {
                "broken" => Fault::new(code!("agent.io_failed"), "The agent's pipes failed"),
                "stuck" => return std::future::pending().await,
                _ => Fault::new(code!("agent.busy"), "The agent is still on a turn"),
            })
        }

        async fn answer(&mut self, _answer: &PermissionAnswer) -> Result<(), Fault> {
            Ok(())
        }

        async fn interrupt(&mut self) -> Result<(), Fault> {
            Ok(())
        }

        async fn next(&mut self) -> Result<Option<RunEvent>, Fault> {
            if self.said {
                return std::future::pending().await;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
            self.said = true;
            let text = "Last words.".to_owned();
            Ok(Some(RunEvent::Text { text }))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_whose_pipes_break_is_stopped_once_it_has_said_its_last_words() {
        let of = RunOf {
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            chat: ChatId::try_from("c-1".to_owned()).unwrap(),
            run: RunId::try_from("run-1".to_owned()).unwrap(),
        };
        let (orders, ordered) = mpsc::unbounded_channel();
        let (reporter, mut reports) = mpsc::channel(8);
        orders.send(Order::Send("busy".to_owned())).unwrap();
        orders.send(Order::Send("broken".to_owned())).unwrap();

        let driven = tokio::spawn(drive(Refuses { said: false }, of, ordered, reporter));

        let said = reports.recv().await.unwrap();
        assert!(matches!(said.heard, Heard::Event(RunEvent::Text { .. })));
        // Still speaking, it takes no more orders.
        assert!(!driven.is_finished() && orders.is_closed());
        let last = reports.recv().await.unwrap();
        assert!(
            matches!(&last.heard, Heard::Exited(Some(fault)) if fault.code().as_str() == "agent.io_failed")
        );
        assert!(reports.recv().await.is_none());
        tokio::time::timeout(LAST_WORDS, driven)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn an_agent_that_stops_reading_its_orders_is_stopped() {
        let of = RunOf {
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            chat: ChatId::try_from("c-1".to_owned()).unwrap(),
            run: RunId::try_from("run-1".to_owned()).unwrap(),
        };
        let (orders, ordered) = mpsc::unbounded_channel();
        let (reporter, mut reports) = mpsc::channel(8);
        orders.send(Order::Send("stuck".to_owned())).unwrap();

        let driven = drive(Refuses { said: true }, of, ordered, reporter);
        tokio::time::timeout(ORDER_WRITE + LAST_WORDS * 2, driven)
            .await
            .unwrap();

        let last = reports.recv().await.unwrap();
        assert!(
            matches!(&last.heard, Heard::Exited(Some(fault)) if fault.code().as_str() == "agent.io_failed")
        );
    }

    // A real process: a shell script stands in for `claude`.
    #[cfg(unix)]
    mod with_a_program {
        use std::path::Path;
        use std::time::Duration;

        use weacon_agents::{Launch, SpawnPolicy};
        use weacon_protocol::{AccessLevel, AgentKind, AgentSession, Outcome};
        use weacon_testing::TempDir;

        use super::super::*;
        use crate::test_support::VERSION;

        const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

        // Reads a prompt, says hello, then waits for one more line before its turn fails.
        const SCRIPT: &str = r#"#!/bin/sh
read -r prompt
printf '%s\n' '{"type":"system","subtype":"init","session_id":"0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d"}'
printf '%s\n' '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hello."}]}}'
read -r interrupt
printf '%s\n' '{"type":"result","subtype":"error_during_execution","is_error":true}'
"#;

        // Written from a child shell, so this process never holds a write handle that another
        // test's fork could inherit, which makes running it fail as busy.
        fn write_program(path: &Path, script: &str) {
            let written = std::process::Command::new("sh")
                .args([
                    "-c",
                    "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
                    "sh",
                    script,
                ])
                .arg(path)
                .status()
                .unwrap();
            assert!(written.success());
        }

        async fn next(agent: &mut AgentRun) -> Option<RunEvent> {
            let event = tokio::time::timeout(Duration::from_secs(30), Agent::next(agent));
            event.await.unwrap().unwrap()
        }

        #[tokio::test]
        async fn the_drivers_run_takes_orders_and_says_what_its_agent_does() {
            let dir = TempDir::new().unwrap();
            let program = dir.join("claude");
            write_program(&program, SCRIPT);
            let launch = Launch {
                agent: AgentKind::Claude,
                program,
                config_dir: dir.join("config"),
                cwd: dir.to_path_buf(),
                access: AccessLevel::AskFirst,
                hook: None,
            };
            let run = RunId::try_from("run-1".to_owned()).unwrap();
            let app = weacon_appkit::build(&VERSION);
            let mut agent = launch
                .start(&SpawnPolicy::default(), &app, run.clone(), None)
                .unwrap();
            let answer = PermissionAnswer {
                run,
                prompt: 1,
                allow: true,
            };

            Agent::send(&mut agent, "Hi").await.unwrap();
            let session = AgentSession::try_from(SESSION.to_owned()).unwrap();
            assert_eq!(next(&mut agent).await, Some(RunEvent::Started { session }));
            let hello = RunEvent::Text {
                text: "Hello.".to_owned(),
            };
            assert_eq!(next(&mut agent).await, Some(hello));
            let unasked = Agent::answer(&mut agent, &answer).await.unwrap_err();
            assert_eq!(unasked.code().as_str(), "agent.no_such_prompt");
            Agent::interrupt(&mut agent).await.unwrap();

            let outcome = Outcome::Interrupted;
            assert_eq!(next(&mut agent).await, Some(RunEvent::Finished { outcome }));
            assert_eq!(next(&mut agent).await, None);
        }
    }
}
