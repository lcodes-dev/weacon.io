//! What a move of a repository's base commit did to the anchors of its workspace's docs, sent
//! as proposals for a person to accept: no agent is started for it.

use weacon_codemap::{Index, Moved, Pin};
use weacon_model::{DocText, Title};
use weacon_observability::Fault;
use weacon_protocol::{
    AnchorChange, Batched, CommitId, DocAnchors, DocContent, DocPin, PinnedDoc, ProposalAction,
    ProposalAnswerKind, ProposalChange, ProposalQueryKind, RepoId, StaleAnchor, WorkspaceId, code,
};

use super::{Proposals, proposal_id};
use weacon_protocol::MAX_PROPOSAL;

/// Who the proposals of a move are by, in their ids.
const BY: &str = "moves";

/// A pin a move left the doc's text true to but must now read as `to`.
struct Reanchor {
    from: DocPin,
    to: DocPin,
}

/// A move of the base of the repository `repo`, from the bases `was` to the code `now`, at
/// `commit`, whose `history` says which commits it holds.
pub(crate) struct BaseMove<'a, H> {
    pub(crate) repo: &'a RepoId,
    pub(crate) was: &'a [&'a Index],
    pub(crate) now: &'a Index,
    pub(crate) commit: &'a CommitId,
    pub(crate) history: &'a H,
}

/// The history of the commit a repository's base moved to.
pub(crate) trait History: Sync {
    /// Whether `commit` is the base's commit or one before it; false when that can't be told.
    fn holds(&self, commit: &CommitId) -> impl Future<Output = bool> + Send;
}

/// Proposes, for each of `workspace`'s docs that pin code, what `moved` did to its anchors: a
/// re-anchor and a "needs an update" proposal, as one batch. Each pin moves from the first of
/// the bases it was taken from, as `pin_moved` says. Gives how many docs it proposed for; a doc
/// refused doesn't stop the others, and the first refusal is the error once all are tried.
pub(crate) async fn moved(
    proposals: &Proposals,
    workspace: &WorkspaceId,
    moved: &BaseMove<'_, impl History>,
) -> Result<usize, Fault> {
    let (mut after, mut batches, mut refused) = (None, 0, None);
    loop {
        let page = proposals
            .query(workspace, ProposalQueryKind::Pinned { after })
            .await?;
        let ProposalAnswerKind::Pinned { docs, more } = page else {
            return Err(unexpected());
        };
        for doc in &docs {
            match proposed(proposals, workspace, doc, moved).await {
                Ok(true) => batches += 1,
                Ok(false) => {}
                Err(fault) => refused = refused.or(Some(fault)),
            }
        }
        let Some(last) = docs.last().filter(|_| more) else {
            return refused.map_or(Ok(batches), Err);
        };
        after = Some(last.id.clone());
    }
}

/// Sends the batch of one doc whose pins moved, split where it would be larger than a
/// proposal may be; whether there was one.
async fn proposed(
    proposals: &Proposals,
    workspace: &WorkspaceId,
    doc: &PinnedDoc,
    moved: &BaseMove<'_, impl History>,
) -> Result<bool, Fault> {
    let changes = batch(proposals, workspace, doc, moved).await?;
    if changes.is_empty() {
        return Ok(false);
    }
    let batches = split(changes, MAX_PROPOSAL);
    for proposals_of_doc in batches {
        let action = ProposalAction::Batch {
            proposals: proposals_of_doc,
        };
        proposals.command(workspace, action).await?;
    }
    Ok(true)
}

/// `changes` as one batch, or one batch each when together they are more than `limit` bytes.
fn split(changes: Vec<Batched>, limit: usize) -> Vec<Vec<Batched>> {
    let size = |batched: &Batched| {
        serde_json::to_vec(&batched.change).map_or(usize::MAX, |json| json.len())
    };
    let total = changes.iter().map(size).fold(0, usize::saturating_add);
    if total > limit {
        changes.into_iter().map(|batched| vec![batched]).collect()
    } else {
        vec![changes]
    }
}

/// The proposals of one doc whose pins `moved`.
async fn batch(
    proposals: &Proposals,
    workspace: &WorkspaceId,
    doc: &PinnedDoc,
    moved: &BaseMove<'_, impl History>,
) -> Result<Vec<Batched>, Fault> {
    let anchors = &doc.anchors;
    // Pins taken in the base's history are of this repository, whatever their id names here.
    let ours =
        anchors.repo.as_ref() == Some(moved.repo) && moved.history.holds(&anchors.commit).await;
    let (reanchors, mut stale) = findings(anchors, moved, ours);
    let mut changes = Vec::new();
    if !reanchors.is_empty() {
        let query = ProposalQueryKind::Doc {
            doc: doc.id.clone(),
        };
        let ProposalAnswerKind::Doc { doc: record } = proposals.query(workspace, query).await?
        else {
            return Err(unexpected());
        };
        let content = (record.version == doc.version)
            .then(|| reanchored(&record.content, &reanchors, moved))
            .flatten();
        match content {
            Some(content) => changes.push(ProposalChange::DocEdit {
                doc: doc.id.clone(),
                base: doc.version,
                content,
            }),
            // A move is seen once, so a rename the doc can't take now needs an update.
            None => stale.extend(reanchors.into_iter().map(|Reanchor { from, to }| {
                let change = AnchorChange::Changed {
                    was: from.anchor().to_owned(),
                    now: to.anchor().to_owned(),
                };
                StaleAnchor { pin: from, change }
            })),
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

/// What `moved` did to the pins of `anchors`, which are `ours` when known to be of its
/// repository: those to re-anchor, and those whose code changed or went; nothing when they say
/// they are of another repository.
fn findings<H>(
    anchors: &DocAnchors,
    moved: &BaseMove<'_, H>,
    ours: bool,
) -> (Vec<Reanchor>, Vec<StaleAnchor>) {
    let (mut reanchors, mut stale) = (Vec::new(), Vec::new());
    if anchors.repo.as_ref().is_some_and(|repo| repo != moved.repo) {
        return (reanchors, stale);
    }
    for from in &anchors.pins {
        let Ok(pin) = from.as_str().parse::<Pin>() else {
            continue;
        };
        let change = match pin_moved(&pin, moved, ours) {
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

/// What `moved` did to `pin`, from the first of its bases it was taken from, else from the
/// first that holds its anchor, else from the first. When none was taken from, code that
/// differs from the pin has changed; an anchor no index holds is gone when the pin is `ours`,
/// and may be another repository's otherwise.
fn pin_moved<H>(pin: &Pin, moved: &BaseMove<'_, H>, ours: bool) -> Option<Moved> {
    let BaseMove { was, now, .. } = moved;
    let taken = |base: &&&Index| base.fingerprint(&pin.anchor) == Some(pin.fingerprint);
    if let Some(base) = was.iter().find(taken) {
        return base.after_move(now, pin);
    }
    let holds = |base: &&&Index| base.fingerprint(&pin.anchor).is_some();
    let moved = was
        .iter()
        .find(holds)
        .or_else(|| was.first())?
        .after_move(now, pin);
    moved.or_else(|| {
        let anchor = pin.anchor.to_string();
        match now.fingerprint(&pin.anchor) {
            Some(code) => (code != pin.fingerprint).then(|| Moved::Changed {
                was: anchor.clone(),
                now: anchor,
            }),
            None => ours.then_some(Moved::Gone { was: anchor }),
        }
    })
}

/// `content` with each of `reanchors` made, its links to a renamed anchor relinked, and its
/// pins said to be of `moved`'s repository at its commit; none when the result isn't a doc the
/// server would take.
fn reanchored(
    content: &DocContent,
    reanchors: &[Reanchor],
    moved: &BaseMove<'_, impl History>,
) -> Option<DocContent> {
    let anchors = content.anchors.as_ref()?;
    let pairs = reanchors.iter().map(|Reanchor { from, to }| (from, to));
    let body = weacon_model::reanchored(&content.body, pairs);
    let pins = anchors
        .pins
        .iter()
        .map(|pin| {
            let moved = reanchors.iter().find(|reanchor| reanchor.from == *pin);
            moved.map_or_else(|| pin.clone(), |reanchor| reanchor.to.clone())
        })
        .collect();
    let anchors = DocAnchors {
        commit: moved.commit.clone(),
        pins,
        repo: Some(moved.repo.clone()),
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
                    repo: None,
                }),
            },
        }
    }

    fn workspace() -> WorkspaceId {
        "w-1".parse().unwrap()
    }

    /// A history that holds every commit, or none.
    struct Held(bool);

    impl History for Held {
        async fn holds(&self, _: &CommitId) -> bool {
            self.0
        }
    }

    /// A move of `site`'s base whose history holds every commit.
    fn site_move<'a>(
        repo: &'a RepoId,
        commit: &'a CommitId,
        was: &'a [&'a Index],
        now: &'a Index,
    ) -> BaseMove<'a, Held> {
        BaseMove {
            repo,
            was,
            now,
            commit,
            history: &Held(true),
        }
    }

    fn site() -> RepoId {
        "site".parse().unwrap()
    }

    fn anchors(pins: &[DocPin], repo: Option<&str>) -> DocAnchors {
        DocAnchors {
            commit: commit('a'),
            pins: pins.to_vec(),
            repo: repo.map(|repo| repo.parse().unwrap()),
        }
    }

    /// What moving `site` from `was` to `now` did to `anchors`.
    fn found(
        anchors: &DocAnchors,
        was: &[&Index],
        now: &Index,
    ) -> (Vec<Reanchor>, Vec<StaleAnchor>) {
        let (repo, commit) = (site(), commit('b'));
        let moved = site_move(&repo, &commit, was, now);
        findings(anchors, &moved, anchors.repo == Some(site()))
    }

    /// Proposes what moving `site` from `was` to `now`, at `commit`, did to the docs.
    async fn checked(
        proposals: &Proposals,
        was: &[&Index],
        now: &Index,
        commit: &CommitId,
    ) -> Result<usize, Fault> {
        let repo = site();
        let moved = site_move(&repo, commit, was, now);
        super::moved(proposals, &workspace(), &moved).await
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
        let after = repo("mod m;\npub fn run() { go() }\npub fn go() {}\n", "");
        let fake = Fake::default();
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &["sym:a_b::m::go"]));
        let proposals = fake.connect();

        let docs = checked(&proposals, &[&before], &after, &commit('b'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((docs, commands.len()), (1, 1));
        let content = DocContent {
            title: "Plan".to_owned(),
            body: "See [it](sym:a_b::go).\n".to_owned(),
            anchors: Some(DocAnchors {
                commit: commit('b'),
                pins: vec![pin(&after, "sym:a_b::go")],
                repo: Some(site()),
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
            "mod m;\npub fn run() { go(); go() }\npub fn go() {}\n",
            "pub struct S { x: u8 }\n",
        );
        let fake = Fake::default();
        let anchors = ["sym:a_b::m::go", "sym:a_b::m::S", "sym:a_b::m::T"];
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &anchors));
        let proposals = fake.connect();
        let line = "struct a_b::m::S  src/m.rs:2".to_owned();

        let docs = checked(&proposals, &[&before], &after, &commit('b'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((docs, commands.len()), (1, 1));
        let first = changes(&commands[0]);
        let relinked = |body: &str| body.contains("sym:a_b::go") && body.contains("sym:a_b::m::T");
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
                            was: line,
                            now: "struct a_b::m::S  src/m.rs:1".to_owned(),
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

        let batches = checked(&proposals, &[&before], &after, &commit('b'))
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
        let after = repo("mod m;\npub fn run() { go() }\npub fn go() {}\n", "");
        let later = repo(
            "mod m;\npub fn run() { go() }\npub fn go() {}\n",
            "pub fn more() {}\n",
        );
        let (fake, other) = (Fake::default(), Fake::default());
        for fake in [&fake, &other] {
            let pinned = doc("d-1", &before, &["sym:a_b::m::go"]);
            fake.docs.lock().unwrap().push(pinned);
        }
        let proposals = fake.connect();

        checked(&proposals, &[&before], &after, &commit('b'))
            .await
            .unwrap();
        let again = checked(&proposals, &[&after], &later, &commit('c'))
            .await
            .unwrap();
        // Another core that sees the same move.
        checked(&other.connect(), &[&before], &after, &commit('b'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((again, commands.len()), (0, 1));
        assert_eq!(commands[0].action, other.commands()[0].action);
    }

    #[tokio::test]
    async fn a_second_move_of_a_symbol_whose_re_anchor_is_open_needs_an_update() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { go() }\npub fn go() {}\n", "");
        let later = repo("mod m;\npub fn run() { go() }\npub fn go() { run() }\n", "");
        let fake = Fake::default();
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &["sym:a_b::m::go"]));
        let proposals = fake.connect();

        checked(&proposals, &[&before], &after, &commit('b'))
            .await
            .unwrap();
        let again = checked(&proposals, &[&after], &later, &commit('c'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((again, commands.len()), (1, 2));
        let stale = StaleAnchor {
            pin: pin(&after, "sym:a_b::go"),
            change: AnchorChange::Changed {
                was: "fn a_b::go  src/lib.rs:3".to_owned(),
                now: "fn a_b::go  src/lib.rs:3".to_owned(),
            },
        };
        assert_eq!(
            changes(&commands[1]),
            [ProposalChange::NeedsUpdate {
                doc: "d-1".parse().unwrap(),
                base: 3,
                anchors: vec![stale],
            }]
        );
    }

    #[tokio::test]
    async fn a_refused_re_anchor_is_proposed_again_with_the_same_move() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { go() }\npub fn go() {}\n", "");
        let fake = Fake::default();
        fake.docs
            .lock()
            .unwrap()
            .push(doc("d-1", &before, &["sym:a_b::m::go"]));
        *fake.refuse.lock().unwrap() = Some(code!("proposal.too_many"));
        let proposals = fake.connect();

        checked(&proposals, &[&before], &after, &commit('b'))
            .await
            .unwrap_err();
        *fake.refuse.lock().unwrap() = None;
        let again = checked(&proposals, &[&before], &after, &commit('b'))
            .await
            .unwrap();

        let commands = fake.commands();
        assert_eq!((again, commands.len()), (1, 2));
        assert_eq!(commands[0].action, commands[1].action);
    }

    #[tokio::test]
    async fn a_rename_the_doc_cant_take_now_needs_an_update() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { go() }\npub fn go() {}\n", "");
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

        let renamed = StaleAnchor {
            pin: pin(&before, "sym:a_b::m::go"),
            change: AnchorChange::Changed {
                was: "sym:a_b::m::go".to_owned(),
                now: "sym:a_b::go".to_owned(),
            },
        };
        let changes: Vec<ProposalChange> = made.into_iter().map(|batched| batched.change).collect();
        assert_eq!(
            changes,
            [ProposalChange::NeedsUpdate {
                doc: "d-1".parse().unwrap(),
                base: 3,
                anchors: vec![renamed],
            }]
        );
    }

    #[tokio::test]
    async fn each_pin_moves_from_the_base_it_was_taken_from() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { go() }\npub fn go() {}\n", "");
        let later = repo("mod m;\npub fn run() { go() }\npub fn go() { run() }\n", "");
        let fake = Fake::default();
        // Pinned from code neither base holds, so it moves from the oldest.
        let elsewhere = repo(LIB, "pub fn go() { let x = 1; }\n");
        let docs = [
            doc("d-1", &before, &["sym:a_b::m::go"]),
            doc("d-2", &after, &["sym:a_b::go"]),
            doc("d-3", &elsewhere, &["sym:a_b::m::go"]),
        ];
        fake.docs.lock().unwrap().extend(docs);
        let proposals = fake.connect();

        let batches = checked(&proposals, &[&before, &after], &later, &commit('c'))
            .await
            .unwrap();

        let about: Vec<String> = fake
            .commands()
            .iter()
            .flat_map(changes)
            .map(|change| about(&change))
            .collect();
        let docs = ["d-1", "d-2", "d-3"].map(str::to_owned).to_vec();
        assert_eq!((batches, about), (3, docs));
    }

    #[tokio::test]
    async fn a_pin_no_base_was_taken_from_moves_from_one_holding_its_anchor_or_has_changed() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() { go() }\npub fn go() {}\n", "");
        let later = repo("mod m;\npub fn run() { go() }\npub fn go() { run() }\n", "");
        let elsewhere = repo(
            "mod m;\npub fn run() { go() }\npub fn go() -> u8 { 1 }\n",
            "",
        );
        let pins = anchors(&[pin(&elsewhere, "sym:a_b::go")], None);
        let moved = |was: &[&Index], now: &Index| found(&pins, was, now).1;
        let change = |stale: Vec<StaleAnchor>| {
            stale
                .into_iter()
                .map(|stale| stale.change)
                .collect::<Vec<_>>()
        };

        // The oldest base has no `go` of the crate's root; the next has it, and it changed.
        assert_eq!(
            change(moved(&[&before, &after], &later)),
            [AnchorChange::Changed {
                was: "fn a_b::go  src/lib.rs:3".to_owned(),
                now: "fn a_b::go  src/lib.rs:3".to_owned(),
            }]
        );
        // Its base was let go, and no base kept says when its code changed.
        let anchor = "sym:a_b::go".to_owned();
        assert_eq!(
            change(moved(&[&before, &later], &later)),
            [AnchorChange::Changed {
                was: anchor.clone(),
                now: anchor
            }]
        );
        assert_eq!(moved(&[&before, &elsewhere], &elsewhere), []);
        // Pinned in another repository, which no base here or the code now holds.
        assert_eq!(moved(&[&before], &before), []);
    }

    #[tokio::test]
    async fn a_refused_doc_leaves_the_others_to_be_proposed() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo(LIB, "pub fn go() { let x = 1; }\n");
        let fake = Fake::default();
        let docs = [
            doc("d-1", &before, &["sym:a_b::m::go"]),
            doc("d-2", &before, &["sym:a_b::m::go"]),
        ];
        fake.docs.lock().unwrap().extend(docs);
        *fake.refuse.lock().unwrap() = Some(code!("proposal.too_many"));
        let proposals = fake.connect();

        let refused = checked(&proposals, &[&before], &after, &commit('b')).await;

        assert_eq!(refused.unwrap_err().code(), &code!("proposal.too_many"));
        assert_eq!(fake.commands().len(), 2);
    }

    #[test]
    fn a_batch_larger_than_a_proposal_goes_one_proposal_at_a_time() {
        let batched = |title: &str| Batched {
            proposal: "p-1".parse().unwrap(),
            change: ProposalChange::NewTask {
                title: title.to_owned(),
            },
        };
        let both = || vec![batched("a"), batched("b")];
        let size = serde_json::to_vec(&both()[0].change).unwrap().len();

        assert_eq!(split(both(), 2 * size), [both()]);
        assert_eq!(
            split(both(), 2 * size - 1),
            [vec![batched("a")], vec![batched("b")]]
        );
    }

    async fn batch_of(
        proposals: &Proposals,
        doc: &PinnedDoc,
        (was, now): (&Index, &Index),
    ) -> Vec<Batched> {
        let (repo, commit) = (site(), commit('b'));
        let was = [was];
        let moved = site_move(&repo, &commit, &was, now);
        batch(proposals, &workspace(), doc, &moved).await.unwrap()
    }

    #[tokio::test]
    async fn a_rename_to_a_pin_no_doc_can_hold_needs_an_update() {
        let long = "a".repeat(600);
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo(
            &format!(
                "mod m;\npub fn run() {{ {long}::go() }}\nmod {long} {{ pub fn go() {{}} }}\n"
            ),
            "",
        );
        let pins = [pin(&before, "sym:a_b::m::go")];

        let (reanchors, stale) = found(&anchors(&pins, None), &[&before], &after);

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

        let (repo, commit) = (site(), commit('b'));
        let moved = site_move(&repo, &commit, &[], &index);
        assert_eq!(reanchored(&content, &reanchors, &moved), None);
    }
    #[test]
    fn a_pin_of_this_repository_that_no_index_holds_is_gone_and_another_repositorys_is_left_alone()
    {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() {}\n", "");
        let changed = repo(LIB, "pub fn go() -> u8 { 1 }\n");
        let pins = [pin(&before, "sym:a_b::m::go")];
        let gone = |anchors: &DocAnchors, was: &[&Index], now: &Index| {
            let (reanchors, stale) = found(anchors, was, now);
            assert!(reanchors.is_empty());
            stale
                .into_iter()
                .map(|stale| stale.change)
                .collect::<Vec<_>>()
        };

        // The base it was taken from was let go.
        assert_eq!(
            gone(&anchors(&pins, Some("site")), &[&after], &after),
            [AnchorChange::Gone {
                was: "sym:a_b::m::go".to_owned()
            }]
        );
        assert_eq!(gone(&anchors(&pins, None), &[&after], &after), []);
        for (was, now) in [(&before, &after), (&before, &changed), (&after, &after)] {
            assert_eq!(gone(&anchors(&pins, Some("other")), &[was], now), []);
        }
    }

    #[tokio::test]
    async fn a_symbol_renamed_unchanged_is_reanchored_and_one_also_edited_needs_an_update() {
        let before = repo(LIB, "pub fn go() {}\n");
        let renamed = repo("mod m;\npub fn run() { m::went() }\n", "pub fn went() {}\n");
        let edited = repo(
            "mod m;\npub fn run() { went() }\npub fn went() -> u8 { 1 }\n",
            "",
        );
        let pins = anchors(&[pin(&before, "sym:a_b::m::go")], None);

        let (reanchors, stale) = found(&pins, &[&before], &renamed);
        assert!(stale.is_empty());
        let to: Vec<&DocPin> = reanchors.iter().map(|reanchor| &reanchor.to).collect();
        assert_eq!(to, [&pin(&renamed, "sym:a_b::m::went")]);

        let (reanchors, stale) = found(&pins, &[&before], &edited);
        assert!(reanchors.is_empty());
        assert_eq!(
            stale[0].change,
            AnchorChange::Gone {
                was: "fn a_b::m::go  src/m.rs:1".to_owned()
            }
        );
    }

    #[tokio::test]
    async fn a_pin_is_gone_only_when_taken_in_the_history_of_the_base_that_moved() {
        let before = repo(LIB, "pub fn go() {}\n");
        let after = repo("mod m;\npub fn run() {}\n", "");
        let pinned = PinnedDoc {
            id: "d-1".parse().unwrap(),
            version: 3,
            anchors: anchors(&[pin(&before, "sym:a_b::m::go")], Some("site")),
        };
        let proposals = Fake::default().connect();
        let (repo, commit) = (site(), commit('b'));
        let (no, yes, was) = (Held(false), Held(true), [&after]);
        let moved = |history| BaseMove {
            history,
            ..site_move(&repo, &commit, &was, &after)
        };

        let unheld = batch(&proposals, &workspace(), &pinned, &moved(&no)).await;
        let held = batch(&proposals, &workspace(), &pinned, &moved(&yes)).await;

        // On a branch the base hasn't merged, or of another repository named the same here.
        assert!(unheld.unwrap().is_empty());
        assert_eq!(held.unwrap().len(), 1);
    }
}
