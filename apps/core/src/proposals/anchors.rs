//! What a move of a repository's base commit did to the anchors of its workspace's docs, sent
//! as proposals for a person to accept: no agent is started for it.

use weacon_codemap::{Index, Moved, Pin};
use weacon_model::{DocText, Title, relinked};
use weacon_observability::Fault;
use weacon_protocol::{
    AnchorChange, Batched, CommitId, DocAnchors, DocContent, DocPin, PinnedDoc, ProposalAction,
    ProposalAnswerKind, ProposalChange, ProposalQueryKind, StaleAnchor, WorkspaceId, code,
};

use super::{Proposals, proposal_id};

/// Who the proposals of a move are by, in their ids.
const BY: &str = "moves";

/// A pin a move left the doc's text true to but must now read as `to`.
struct Reanchor {
    from: DocPin,
    to: DocPin,
}

/// Proposes, for each of `workspace`'s docs that pin code, what moving the code from `was` to
/// `now`, at `commit`, did to its anchors: a re-anchor and a "needs an update" proposal, as
/// one batch. Gives how many docs it proposed for.
pub(crate) async fn moved(
    proposals: &Proposals,
    workspace: &WorkspaceId,
    was: &Index,
    now: &Index,
    commit: &CommitId,
) -> Result<usize, Fault> {
    let (mut after, mut batches) = (None, 0);
    loop {
        let page = proposals
            .query(workspace, ProposalQueryKind::Pinned { after })
            .await?;
        let ProposalAnswerKind::Pinned { docs, more } = page else {
            return Err(unexpected());
        };
        for doc in &docs {
            let batch = batch(proposals, workspace, doc, (was, now), commit).await?;
            if !batch.is_empty() {
                let action = ProposalAction::Batch { proposals: batch };
                proposals.command(workspace, action).await?;
                batches += 1;
            }
        }
        after = docs.last().map(|doc| doc.id.clone());
        if !more || after.is_none() {
            return Ok(batches);
        }
    }
}

/// The proposals of one doc whose pins `moved` from one index to the other.
async fn batch(
    proposals: &Proposals,
    workspace: &WorkspaceId,
    doc: &PinnedDoc,
    (was, now): (&Index, &Index),
    commit: &CommitId,
) -> Result<Vec<Batched>, Fault> {
    let (reanchors, stale) = findings(&doc.anchors.pins, was, now);
    let mut changes = Vec::new();
    if !reanchors.is_empty() {
        let query = ProposalQueryKind::Doc {
            doc: doc.id.clone(),
        };
        let ProposalAnswerKind::Doc { doc: record } = proposals.query(workspace, query).await?
        else {
            return Err(unexpected());
        };
        // A doc revised since its pins were read is checked again at the next move.
        if record.version == doc.version
            && let Some(content) = reanchored(&record.content, &reanchors, commit)
        {
            changes.push(ProposalChange::DocEdit {
                doc: doc.id.clone(),
                base: doc.version,
                content,
            });
        }
    }
    if !stale.is_empty() {
        changes.push(ProposalChange::NeedsUpdate {
            doc: doc.id.clone(),
            base: doc.version,
            anchors: stale,
        });
    }
    changes
        .into_iter()
        .map(|change| {
            let proposal = proposal_id(workspace, BY, &change)?;
            Ok(Batched { proposal, change })
        })
        .collect()
}

/// What moving from `was` to `now` did to `pins`: those to re-anchor, and those whose code
/// changed or went.
fn findings(pins: &[DocPin], was: &Index, now: &Index) -> (Vec<Reanchor>, Vec<StaleAnchor>) {
    let (mut reanchors, mut stale) = (Vec::new(), Vec::new());
    for from in pins {
        let Ok(pin) = from.as_str().parse::<Pin>() else {
            continue;
        };
        let change = match was.after_move(now, &pin) {
            None => continue,
            // A pin a doc can't hold, such as one past its length, is as good as gone.
            Some(Moved::Reanchored(to)) => match to.to_string().parse() {
                Ok(to) => {
                    reanchors.push(Reanchor {
                        from: from.clone(),
                        to,
                    });
                    continue;
                }
                Err(_) => AnchorChange::Gone {
                    was: from.anchor().to_owned(),
                },
            },
            Some(Moved::Changed { was, now }) => AnchorChange::Changed { was, now },
            Some(Moved::Gone { was }) => AnchorChange::Gone { was },
        };
        stale.push(StaleAnchor {
            pin: from.clone(),
            change,
        });
    }
    (reanchors, stale)
}

/// `content` with each of `reanchors` made, its links to a renamed anchor relinked, and its
/// pins said to be at `commit`; none when the result isn't a doc the server would take.
fn reanchored(
    content: &DocContent,
    reanchors: &[Reanchor],
    commit: &CommitId,
) -> Option<DocContent> {
    let anchors = content.anchors.as_ref()?;
    let mut body = content.body.clone();
    for Reanchor { from, to } in reanchors {
        if from.anchor() != to.anchor() {
            body = relinked(&body, from.anchor(), to.anchor());
        }
    }
    let pins = anchors
        .pins
        .iter()
        .map(|pin| {
            let moved = reanchors.iter().find(|reanchor| reanchor.from == *pin);
            moved.map_or_else(|| pin.clone(), |reanchor| reanchor.to.clone())
        })
        .collect();
    let anchors = DocAnchors {
        commit: commit.clone(),
        pins,
    };
    let title = Title::try_from(content.title.clone()).ok()?;
    DocText::new(title, body, Some(anchors))
        .ok()
        .map(DocContent::from)
}

fn unexpected() -> Fault {
    Fault::new(
        code!("proposal.unexpected"),
        "The server answered with something else",
    )
}

#[cfg(test)]
mod tests {
    use weacon_codemap::{Anchor, SourceFile, index};
    use weacon_protocol::{DocRecord, ProposalCommand};

    use super::super::fake::Fake;
    use super::*;

    const LIB: &str = "mod m;\npub fn run() { m::go() }\n";

    fn repo(lib: &str, module: &str) -> Index {
        let file = |path: &str, text: &str| SourceFile {
            path: path.to_owned(),
            text: text.to_owned(),
        };
        index(&[
            file("Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("src/lib.rs", lib),
            file("src/m.rs", module),
        ])
        .unwrap()
    }

    fn commit(digit: char) -> CommitId {
        digit.to_string().repeat(40).parse().unwrap()
    }

    fn pin(index: &Index, anchor: &str) -> DocPin {
        let anchor: Anchor = anchor.parse().unwrap();
        let fingerprint = index.fingerprint(&anchor).unwrap();
        Pin {
            anchor,
            fingerprint,
        }
        .to_string()
        .parse()
        .unwrap()
    }

    /// A doc at version 3 that links to each of `anchors`, pinned in `index`.
    fn doc(id: &str, index: &Index, anchors: &[&str]) -> DocRecord {
        let body = anchors.iter().fold(String::new(), |body, anchor| {
            body + "See [it](" + anchor + ").\n"
        });
        DocRecord {
            id: id.parse().unwrap(),
            version: 3,
            content: DocContent {
                title: "Plan".to_owned(),
                body,
                anchors: Some(DocAnchors {
                    commit: commit('a'),
                    pins: anchors.iter().map(|anchor| pin(index, anchor)).collect(),
                }),
            },
        }
    }

    fn workspace() -> WorkspaceId {
        "w-1".parse().unwrap()
    }

    fn changes(command: &ProposalCommand) -> Vec<ProposalChange> {
        let ProposalAction::Batch { proposals } = &command.action else {
            panic!("{command:?}");
        };
        proposals
            .iter()
            .map(|batched| batched.change.clone())
            .collect()
    }

    #[tokio::test]
    async fn a_move_with_a_rename_proposes_one_batch_for_the_doc() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { m::went() }\n", "pub fn went() {}\n");
        let fake = Fake::default();
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &["sym:a_b::m::go"]));
        let proposals = fake.connect();

        let docs = moved(&proposals, &workspace(), &before, &after, &commit('b'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((docs, commands.len()), (1, 1));
        let content = DocContent {
            title: "Plan".to_owned(),
            body: "See [it](sym:a_b::m::went).\n".to_owned(),
            anchors: Some(DocAnchors {
                commit: commit('b'),
                pins: vec![pin(&after, "sym:a_b::m::went")],
            }),
        };
        let edit = ProposalChange::DocEdit {
            doc: "d-1".parse().unwrap(),
            base: 3,
            content,
        };
        assert_eq!(changes(&commands[0]), [edit]);
        assert_eq!(commands[0].workspace, workspace());
    }

    #[tokio::test]
    async fn a_changed_and_a_gone_symbol_need_an_update_beside_a_rename_in_the_same_batch() {
        let before = repo(LIB, "pub fn go() {}\npub struct S;\npub struct T;\n");
        let after = repo(
            "mod m;\npub fn run() { m::went(); m::went() }\n",
            "pub fn went() {}\npub struct S { x: u8 }\n",
        );
        let fake = Fake::default();
        let anchors = ["sym:a_b::m::go", "sym:a_b::m::S", "sym:a_b::m::T"];
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &anchors));
        let proposals = fake.connect();
        let line = "struct a_b::m::S  src/m.rs:2".to_owned();

        let docs = moved(&proposals, &workspace(), &before, &after, &commit('b'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((docs, commands.len()), (1, 1));
        let first = changes(&commands[0]);
        let relinked =
            |body: &str| body.contains("sym:a_b::m::went") && body.contains("sym:a_b::m::T");
        assert!(
            matches!(&first[0], ProposalChange::DocEdit { content, .. } if relinked(&content.body))
        );
        let stale = |anchor: &str, change: AnchorChange| StaleAnchor {
            pin: pin(&before, anchor),
            change,
        };
        assert_eq!(
            first[1],
            ProposalChange::NeedsUpdate {
                doc: "d-1".parse().unwrap(),
                base: 3,
                anchors: vec![
                    stale(
                        "sym:a_b::m::S",
                        AnchorChange::Changed {
                            was: line.clone(),
                            now: line
                        }
                    ),
                    stale(
                        "sym:a_b::m::T",
                        AnchorChange::Gone {
                            was: "struct a_b::m::T  src/m.rs:3".to_owned()
                        }
                    ),
                ],
            }
        );
    }

    fn about(change: &ProposalChange) -> String {
        let ProposalChange::NeedsUpdate { doc, .. } = change else {
            return format!("{change:?}");
        };
        doc.as_str().to_owned()
    }

    #[tokio::test]
    async fn each_doc_whose_code_moved_gets_its_own_batch() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo(
            "mod m;\npub fn run() { m::go(); m::go() }\n",
            "pub fn go() {}\n",
        );
        let fake = Fake::default();
        let docs = [
            doc("d-1", &before, &["sym:a_b::run"]),
            doc("d-2", &before, &["sym:a_b::m::go"]),
            doc("d-3", &before, &["flow:a_b::run->a_b::m::go"]),
        ];
        fake.docs.lock().unwrap().extend(docs);
        let proposals = fake.connect();

        let batches = moved(&proposals, &workspace(), &before, &after, &commit('b'))
            .await
            .unwrap();

        let about: Vec<String> = fake
            .commands()
            .iter()
            .flat_map(changes)
            .map(|change| about(&change))
            .collect();
        assert_eq!(
            (batches, about),
            (2, vec!["d-1".to_owned(), "d-3".to_owned()])
        );
    }

    #[tokio::test]
    async fn a_second_move_proposes_nothing_again_and_the_same_move_names_the_same_proposals() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { m::went() }\n", "pub fn went() {}\n");
        let later = repo(
            "mod m;\npub fn run() { m::went() }\n",
            "pub fn went() {}\npub fn more() {}\n",
        );
        let fake = Fake::default();
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &["sym:a_b::m::go"]));
        let proposals = fake.connect();

        moved(&proposals, &workspace(), &before, &after, &commit('b'))
            .await
            .unwrap();
        let again = moved(&proposals, &workspace(), &after, &later, &commit('c'))
            .await
            .unwrap();
        moved(&proposals, &workspace(), &before, &after, &commit('b'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((again, commands.len()), (0, 2));
        assert_eq!(commands[0].action, commands[1].action);
    }

    #[tokio::test]
    async fn a_doc_revised_since_its_pins_were_read_is_not_reanchored() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { m::went() }\n", "pub fn went() {}\n");
        let fake = Fake::default();
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &["sym:a_b::m::go"]));
        let proposals = fake.connect();
        // The doc changes between the page of pins and the read of its text.
        let pinned = ProposalQueryKind::Pinned { after: None };
        let ProposalAnswerKind::Pinned { docs, .. } =
            proposals.query(&workspace(), pinned).await.unwrap()
        else {
            panic!("no page");
        };
        fake.docs.lock().unwrap()[0].version = 4;

        let made = batch_of(&proposals, &docs[0], (&before, &after)).await;

        assert!(made.is_empty(), "{made:?}");
    }

    async fn batch_of(
        proposals: &Proposals,
        doc: &PinnedDoc,
        indexes: (&Index, &Index),
    ) -> Vec<Batched> {
        batch(proposals, &workspace(), doc, indexes, &commit('b'))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_rename_to_a_pin_no_doc_can_hold_needs_an_update() {
        let long = "a".repeat(600);
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo(
            &format!("mod m;\npub fn run() {{ m::{long}() }}\n"),
            &format!("pub fn {long}() {{}}\n"),
        );
        let pins = [pin(&before, "sym:a_b::m::go")];

        let (reanchors, stale) = findings(&pins, &before, &after);

        assert!(reanchors.is_empty());
        let gone = AnchorChange::Gone {
            was: "sym:a_b::m::go".to_owned(),
        };
        assert_eq!(
            stale,
            [StaleAnchor {
                pin: pins[0].clone(),
                change: gone
            }]
        );
    }

    #[test]
    fn a_reanchor_that_leaves_the_doc_unreadable_is_dropped() {
        let index = repo(LIB, "pub fn go() {}\n");
        let content = doc("d-1", &index, &["sym:a_b::m::go", "sym:a_b::run"]).content;
        // Both pins would read as one anchor, which no doc may pin twice.
        let reanchors = [Reanchor {
            from: pin(&index, "sym:a_b::m::go"),
            to: pin(&index, "sym:a_b::run"),
        }];

        assert_eq!(reanchored(&content, &reanchors, &commit('b')), None);
    }
}
