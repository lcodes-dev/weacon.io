//! A fake of the server's side of proposals, speaking the protocol's types: it keeps each
//! command a core sends, and answers queries from the docs it holds.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, mpsc};
use weacon_protocol::{
    Code, DocContent, DocRecord, Envelope, Message, Outcome, PinnedDoc, ProposalAction,
    ProposalAnswer, ProposalAnswerKind, ProposalChange, ProposalCommand, ProposalQuery,
    ProposalQueryKind, Receipt, code,
};

use super::{Proposals, channel, serve};

/// What the fake server was sent, and what it answers.
#[derive(Clone, Default)]
pub(crate) struct Fake {
    pub(crate) docs: Arc<Mutex<Vec<DocRecord>>>,
    pub(crate) commands: Arc<Mutex<Vec<ProposalCommand>>>,
    /// The code it refuses each command with, if any.
    pub(crate) refuse: Arc<Mutex<Option<Code>>>,
    /// Told of each command once it is kept.
    pub(crate) heard: Arc<Notify>,
}

impl Fake {
    /// A way to propose to this fake, as through a connected uplink.
    pub(crate) fn connect(&self) -> Proposals {
        let (mut proposals, asks) = channel();
        // So that a test whose answer never comes fails soon.
        proposals.set_patience(Duration::from_secs(1));
        let (to_server, mut sent) = mpsc::channel(8);
        let (answered, answers) = mpsc::channel(8);
        tokio::spawn(serve(asks, to_server, answers));
        let fake = self.clone();
        tokio::spawn(async move {
            while let Some(envelope) = sent.recv().await {
                let message = fake.answer(envelope.message);
                drop(answered.send(Envelope { id: 0, message }).await);
            }
        });
        proposals
    }

    pub(crate) fn commands(&self) -> Vec<ProposalCommand> {
        self.commands.lock().unwrap().clone()
    }

    fn answer(&self, message: Message) -> Message {
        if let Message::ProposalQuery(query) = message {
            return Message::ProposalAnswer(self.query(query));
        }
        let Message::ProposalCommand(command) = message else {
            return Message::Unknown;
        };
        let refuse = self.refuse.lock().unwrap().clone();
        let outcome = refuse.map_or(Outcome::Completed, |code| Outcome::Failed { code });
        let request = command.request.clone();
        self.commands.lock().unwrap().push(command);
        self.heard.notify_one();
        Message::Receipt(Receipt { request, outcome })
    }

    /// What the latest re-anchor this core sent from `doc`'s revision makes it, as
    /// the server applies a machine's own open re-anchor to what it answers that machine.
    fn own_edit(&self, doc: &DocRecord) -> Option<DocContent> {
        let commands = self.commands.lock().unwrap();
        let batched = commands.iter().rev().flat_map(|command| {
            let ProposalAction::Batch { proposals } = &command.action else {
                return Vec::new();
            };
            proposals.iter().map(|batched| &batched.change).collect()
        });
        batched.into_iter().find_map(|change| {
            let ProposalChange::DocEdit {
                doc: id,
                base,
                content,
            } = change
            else {
                return None;
            };
            (*id == doc.id && *base == doc.version).then(|| content.clone())
        })
    }

    fn query(&self, ProposalQuery { request, query, .. }: ProposalQuery) -> ProposalAnswer {
        let docs: Vec<DocRecord> = self
            .docs
            .lock()
            .unwrap()
            .iter()
            .map(|doc| {
                let mut doc = doc.clone();
                if let Some(content) = self.own_edit(&doc) {
                    doc.content = content;
                }
                doc
            })
            .collect();
        let answer = match query {
            // A page holds one doc, so a core reads them page by page.
            ProposalQueryKind::Pinned { after } => {
                let mut pinned = docs.iter().filter_map(|doc| {
                    let later = after
                        .as_ref()
                        .is_none_or(|after| doc.id.as_str() > after.as_str());
                    Some(PinnedDoc {
                        id: doc.id.clone(),
                        version: doc.version,
                        anchors: doc.content.anchors.clone().filter(|_| later)?,
                    })
                });
                let docs = pinned.next().into_iter().collect();
                ProposalAnswerKind::Pinned {
                    docs,
                    more: pinned.next().is_some(),
                }
            }
            ProposalQueryKind::Doc { doc } => match docs.iter().find(|record| record.id == doc) {
                Some(doc) => ProposalAnswerKind::Doc { doc: doc.clone() },
                None => ProposalAnswerKind::Failed {
                    code: code!("doc.not_found"),
                },
            },
            ProposalQueryKind::Newer => ProposalAnswerKind::Newer,
        };
        ProposalAnswer { request, answer }
    }
}
