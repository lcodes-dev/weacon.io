//! What the core proposes to the docs and tasks of its owner's workspaces, and what it asks the
//! server to propose it: it sends each over the uplink, and waits for the server's answer.

mod anchors;
#[cfg(test)]
pub(crate) mod fake;

pub(crate) use anchors::moved;

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use weacon_observability::Fault;
use weacon_protocol::{
    Code, Envelope, MAX_PROPOSAL, Message, Outcome, ProposalAction, ProposalAnswerKind,
    ProposalChange, ProposalCommand, ProposalId, ProposalQuery, ProposalQueryKind, RequestId,
    WorkspaceId, code, to_hex,
};

/// How many asks wait for the uplink to take them.
const QUEUE: usize = 16;
/// How long an ask waits for the server's answer; the uplink may be down.
const PATIENCE: Duration = Duration::from_secs(30);

/// Sends proposals and their queries to the server, from anywhere in the core.
#[derive(Clone, Debug)]
pub(crate) struct Proposals {
    asks: mpsc::Sender<Ask>,
    last: Arc<AtomicU64>,
    patience: Duration,
}

/// The other end of [`Proposals`], which [`serve`] takes.
#[derive(Debug)]
pub(crate) struct Asks(mpsc::Receiver<Ask>);

#[derive(Debug)]
struct Ask {
    request: RequestId,
    message: Message,
    answer: oneshot::Sender<Message>,
}

/// A way to send proposals, and the asks it makes.
pub(crate) fn channel() -> (Proposals, Asks) {
    let (asks, taken) = mpsc::channel(QUEUE);
    let proposals = Proposals {
        asks,
        last: Arc::default(),
        patience: PATIENCE,
    };
    (proposals, Asks(taken))
}

impl Proposals {
    /// Sends `action` to `workspace`, and waits for the server to store it.
    pub(crate) async fn command(
        &self,
        workspace: &WorkspaceId,
        action: ProposalAction,
    ) -> Result<(), Fault> {
        let request = self.request()?;
        let command = ProposalCommand {
            request: request.clone(),
            workspace: workspace.clone(),
            action,
        };
        let Message::Receipt(receipt) =
            self.ask(request, Message::ProposalCommand(command)).await?
        else {
            return Err(unsent());
        };
        if let Outcome::Failed { code } = receipt.outcome {
            return Err(refused(code));
        }
        matches!(receipt.outcome, Outcome::Completed)
            .then_some(())
            .ok_or_else(unsent)
    }

    /// The server's answer to `query` about `workspace`.
    pub(crate) async fn query(
        &self,
        workspace: &WorkspaceId,
        query: ProposalQueryKind,
    ) -> Result<ProposalAnswerKind, Fault> {
        let request = self.request()?;
        let query = ProposalQuery {
            request: request.clone(),
            workspace: workspace.clone(),
            query,
        };
        let Message::ProposalAnswer(answer) =
            self.ask(request, Message::ProposalQuery(query)).await?
        else {
            return Err(unsent());
        };
        if let ProposalAnswerKind::Failed { code } = answer.answer {
            return Err(refused(code));
        }
        Ok(answer.answer)
    }

    async fn ask(&self, request: RequestId, message: Message) -> Result<Message, Fault> {
        let (answer, answered) = oneshot::channel();
        let ask = Ask {
            request,
            message,
            answer,
        };
        self.asks.try_send(ask).map_err(|_| unsent())?;
        let answered = tokio::time::timeout(self.patience, answered).await;
        answered.map_err(|_| unsent())?.map_err(|_| unsent())
    }

    fn request(&self) -> Result<RequestId, Fault> {
        let number = self.last.fetch_add(1, Ordering::Relaxed);
        RequestId::try_from(format!("proposal-{number}")).map_err(|_| unsent())
    }
}

/// Sends each ask in `asks` to the server through `to_server`, and gives each answer in
/// `answers` to the ask that waits for it; it runs until `asks` closes.
pub(crate) async fn serve(
    Asks(mut asks): Asks,
    to_server: mpsc::Sender<Envelope>,
    mut answers: mpsc::Receiver<Envelope>,
) {
    let mut waiting: HashMap<RequestId, oneshot::Sender<Message>> = HashMap::new();
    loop {
        tokio::select! {
            ask = asks.recv() => {
                let Some(ask) = ask else { return };
                waiting.retain(|_, answer| !answer.is_closed());
                // A full queue drops the ask, whose asker hears so at once.
                let envelope = Envelope { id: 0, message: ask.message };
                if to_server.try_send(envelope).is_ok() {
                    waiting.insert(ask.request, ask.answer);
                }
            }
            Some(answer) = answers.recv() => {
                if let Some(waiter) = request_of(&answer.message).and_then(|request| waiting.remove(request)) {
                    drop(waiter.send(answer.message));
                }
            }
        }
    }
}

/// The request the server's `message` answers, if it answers one of [`serve`]'s asks.
fn request_of(message: &Message) -> Option<&RequestId> {
    if let Message::Receipt(receipt) = message {
        return Some(&receipt.request);
    }
    if let Message::ProposalAnswer(answer) = message {
        return Some(&answer.request);
    }
    None
}

/// Whether `message` is an ask of [`serve`]'s.
pub(crate) const fn asks(message: &Message) -> bool {
    matches!(
        message,
        Message::ProposalCommand(_) | Message::ProposalQuery(_)
    )
}

/// Whether the server's `message` answers an ask of [`serve`]'s.
pub(crate) const fn answers(message: &Message) -> bool {
    matches!(message, Message::Receipt(_) | Message::ProposalAnswer(_))
}

/// The id of the proposal of `change` that `by` makes in `workspace`: the same change sent
/// again by the same one is the same proposal.
pub(crate) fn proposal_id(
    workspace: &WorkspaceId,
    by: &str,
    change: &ProposalChange,
) -> Result<ProposalId, Fault> {
    let json = sized(change)?;
    let mut hash = Sha256::new();
    for part in [workspace.as_str().as_bytes(), by.as_bytes(), &json] {
        hash.update(part);
        hash.update([0]);
    }
    let digest = hash.finalize();
    let hex = to_hex(digest.get(..16).unwrap_or_default());
    ProposalId::try_from(format!("core-{hex}")).map_err(|_| too_large())
}

/// `change` as JSON, if it is no larger than a proposal may be.
fn sized(change: &ProposalChange) -> Result<Vec<u8>, Fault> {
    let json = serde_json::to_vec(change).map_err(|_| too_large())?;
    if json.len() > MAX_PROPOSAL {
        return Err(too_large());
    }
    Ok(json)
}

fn refused(code: Code) -> Fault {
    Fault::new(code, "The server refused the proposal")
}

fn unsent() -> Fault {
    Fault::new(
        code!("proposal.unsent"),
        "The proposal couldn't reach the server",
    )
}

pub(crate) fn too_large() -> Fault {
    Fault::new(
        code!("proposal.too_large"),
        "The proposal is larger than the limit",
    )
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{DocContent, DocRecord, ProposalAnswer, Receipt};

    use super::fake::Fake;
    use super::*;

    fn workspace() -> WorkspaceId {
        "w-1".parse().unwrap()
    }

    fn task() -> ProposalChange {
        ProposalChange::NewTask {
            title: "Ship".to_owned(),
        }
    }

    fn propose() -> ProposalAction {
        ProposalAction::Propose {
            proposal: "p-1".parse().unwrap(),
            agent: weacon_protocol::AgentKind::Claude,
            change: task(),
        }
    }

    #[tokio::test]
    async fn a_command_and_a_query_come_back_with_the_servers_answers() {
        let fake = Fake::default();
        let doc = DocRecord {
            id: "d-1".parse().unwrap(),
            version: 2,
            content: DocContent {
                title: "Plan".to_owned(),
                body: "Plain.\n".to_owned(),
                anchors: None,
            },
        };
        fake.docs.lock().unwrap().push(doc.clone());
        let proposals = fake.connect();

        proposals.command(&workspace(), propose()).await.unwrap();
        let read = proposals
            .query(
                &workspace(),
                ProposalQueryKind::Doc {
                    doc: doc.id.clone(),
                },
            )
            .await
            .unwrap();
        let missing = ProposalQueryKind::Doc {
            doc: "d-9".parse().unwrap(),
        };
        let missing = proposals.query(&workspace(), missing).await.unwrap_err();
        *fake.refuse.lock().unwrap() = Some(code!("proposal.too_many"));
        let refused = proposals
            .command(&workspace(), propose())
            .await
            .unwrap_err();

        assert_eq!(read, ProposalAnswerKind::Doc { doc });
        assert_eq!(missing.code(), &code!("doc.not_found"));
        assert_eq!(refused.code(), &code!("proposal.too_many"));
        let commands = fake.commands();
        assert_eq!(commands.len(), 2);
        assert_eq!(
            (&commands[0].workspace, &commands[0].action),
            (&workspace(), &propose())
        );
        assert_ne!(commands[0].request, commands[1].request);
    }

    #[tokio::test]
    async fn an_ask_nobody_takes_or_answers_is_unsent() {
        let (proposals, asks) = channel();
        drop(asks);
        let gone = proposals
            .command(&workspace(), propose())
            .await
            .unwrap_err();
        let (mut proposals, asks) = channel();
        proposals.patience = Duration::from_millis(10);
        let (to_server, _held) = mpsc::channel(1);
        let (answered, answers) = mpsc::channel(1);
        tokio::spawn(serve(asks, to_server, answers));
        let unanswered = proposals
            .command(&workspace(), propose())
            .await
            .unwrap_err();
        // The server's queue is full, so this one is dropped at once.
        let full = proposals
            .command(&workspace(), propose())
            .await
            .unwrap_err();
        let stranger = Envelope {
            id: 0,
            message: Message::Receipt(Receipt {
                request: "elsewhere".parse().unwrap(),
                outcome: Outcome::Completed,
            }),
        };
        answered.send(stranger).await.unwrap();

        for fault in [gone, unanswered, full] {
            assert_eq!(fault.code(), &code!("proposal.unsent"));
        }
    }

    // An answer of the other kind than `asked` takes, under its request.
    fn wrong_kind(asked: Message) -> Message {
        if let Message::ProposalQuery(query) = asked {
            return Message::Receipt(Receipt {
                request: query.request,
                outcome: Outcome::Completed,
            });
        }
        let Message::ProposalCommand(command) = asked else {
            return Message::Unknown;
        };
        let request = command.request;
        Message::ProposalAnswer(ProposalAnswer {
            request,
            answer: ProposalAnswerKind::Pinned {
                docs: Vec::new(),
                more: false,
            },
        })
    }

    async fn answer_wrongly(mut sent: mpsc::Receiver<Envelope>, answered: mpsc::Sender<Envelope>) {
        while let Some(envelope) = sent.recv().await {
            let message = wrong_kind(envelope.message);
            answered.send(Envelope { id: 0, message }).await.unwrap();
        }
    }

    #[tokio::test]
    async fn an_answer_of_the_wrong_kind_is_unsent() {
        let (proposals, asks) = channel();
        let (to_server, sent) = mpsc::channel(4);
        let (answered, answers) = mpsc::channel(4);
        tokio::spawn(serve(asks, to_server, answers));
        tokio::spawn(answer_wrongly(sent, answered));

        let command = proposals
            .command(&workspace(), propose())
            .await
            .unwrap_err();
        let pinned = ProposalQueryKind::Pinned { after: None };
        let query = proposals.query(&workspace(), pinned).await.unwrap_err();

        assert_eq!(command.code(), &code!("proposal.unsent"));
        assert_eq!(query.code(), &code!("proposal.unsent"));
    }

    #[test]
    fn a_proposal_is_named_by_its_change_and_who_makes_it_and_held_to_a_size() {
        let id = |by: &str, change: &ProposalChange| proposal_id(&workspace(), by, change).unwrap();
        let other = ProposalChange::NewTask {
            title: "Test".to_owned(),
        };
        let large = ProposalChange::NewTask {
            title: "x".repeat(MAX_PROPOSAL),
        };

        assert_eq!(id("run-1", &task()), id("run-1", &task()));
        assert_ne!(id("run-1", &task()), id("run-2", &task()));
        assert_ne!(id("run-1", &task()), id("run-1", &other));
        assert_ne!(
            id("run-1", &task()),
            proposal_id(&"w-2".parse().unwrap(), "run-1", &task()).unwrap()
        );
        assert_eq!(id("run-1", &task()).as_str().len(), 37);
        let fault = proposal_id(&workspace(), "run-1", &large).unwrap_err();
        assert_eq!(fault.code(), &code!("proposal.too_large"));
        assert!(asks(&Message::ProposalCommand(ProposalCommand {
            request: "r".parse().unwrap(),
            workspace: workspace(),
            action: propose(),
        })));
        assert!(!answers(&Message::Unknown) && !asks(&Message::Unknown));
    }
}
