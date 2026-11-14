use sqlx::SqliteConnection;
use weacon_model::{
    Attribution, Doc, DocText, DocTextError, Merge, OrderKey, Task, TaskEdit, Title,
};
use weacon_observability::Fault;
use weacon_protocol::{
    AgentRef, DocContent, DocId, MemberId, MergeHunk, ProposalChange, ProposalId, ProposalOutcome,
    TaskEditBody, TaskId, code,
};

use super::super::docs::refused as revise_refused;
use super::super::tasks::{no_room, refused as edit_refused};
use super::super::{Workspace, failed, parsed, stored, unreadable};
use super::target::{Snapshot, doc_text};
use super::{ACCEPTED, Closing, OPEN, closed, not_found};

/// The most lines times lines a doc edit's merge may compare once the start and end both
/// sides share are set aside, so that accepting one takes a bounded time; a larger merge is
/// answered as a conflict of the whole body.
const MAX_MERGE: usize = 4_000_000;

type OpenRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

/// An open proposal, as accepting it needs it.
struct Open {
    change: ProposalChange,
    snapshot: Option<Snapshot>,
    new_id: Option<String>,
    attribution: Attribution,
}

impl Workspace {
    /// Accepts the open proposal `id` on behalf of `by`, at `at`: the doc or task changes as
    /// it proposes and keeps its author, and the proposal closes. A conflict changes nothing.
    pub(crate) async fn accept(
        &self,
        id: &ProposalId,
        by: &MemberId,
        at: i64,
    ) -> Result<ProposalOutcome, Fault> {
        let mut transaction = self.db.write().await?;
        let open = self.open(&mut transaction, id, by, at).await?;
        // Its own bytes leave the quota first, so that a full workspace can still accept it.
        self.release(&mut transaction, id).await?;
        let outcome = self.make(&mut transaction, open).await?;
        let ProposalOutcome::Accepted { version } = outcome else {
            return Ok(outcome);
        };
        let closing = Closing {
            state: ACCEPTED,
            by: Some(by),
            version: Some(i64::try_from(version).map_err(|_| unreadable())?),
        };
        self.close(&mut transaction, id, closing, by).await?;
        transaction.commit().await.map_err(failed)?;
        Ok(outcome)
    }

    /// Makes `open`'s change, or says what conflicts with it.
    async fn make(
        &self,
        transaction: &mut SqliteConnection,
        open: Open,
    ) -> Result<ProposalOutcome, Fault> {
        let attribution = &open.attribution;
        match open.change {
            ProposalChange::DocEdit { doc, base, content } => {
                let edit = DocChange {
                    doc: &doc,
                    base,
                    content: &content,
                };
                self.accept_doc_edit(transaction, edit, attribution).await
            }
            ProposalChange::TaskEdit { task, edits, .. } => {
                let snapshot = open.snapshot.ok_or_else(unreadable)?;
                let edit = TaskChange {
                    task: &task,
                    snapshot: &snapshot,
                    edits: &edits,
                };
                self.accept_task_edit(transaction, edit, attribution).await
            }
            ProposalChange::NewDoc { content } => {
                let id: DocId = stored(open.new_id.ok_or_else(unreadable)?)?;
                let doc = Doc::new(id, doc_text(&content).map_err(|_| unreadable())?);
                self.insert_doc(transaction, &doc, attribution).await?;
                Ok(ProposalOutcome::Accepted { version: 1 })
            }
            ProposalChange::NewTask { title } => {
                let id: TaskId = stored(open.new_id.ok_or_else(unreadable)?)?;
                let order = self.after_last(transaction).await?;
                let title = Title::try_from(title).map_err(|_| unreadable())?;
                self.insert_task(transaction, &Task::new(id, title, order), attribution)
                    .await?;
                Ok(ProposalOutcome::Accepted { version: 1 })
            }
            ProposalChange::Newer => Err(unreadable()),
        }
    }

    /// The open proposal `id`, with the attribution its change takes once `by` accepts it;
    /// `proposal.not_found` when there's none, and `proposal.closed` when it isn't open.
    async fn open(
        &self,
        transaction: &mut SqliteConnection,
        id: &ProposalId,
        by: &MemberId,
        at: i64,
    ) -> Result<Open, Fault> {
        let (state, change, snapshot, new_id, owner, agent) = sqlx::query_as::<_, OpenRow>(
            "SELECT state, COALESCE(change, ''), snapshot, new_id, author_member, agent
             FROM proposals WHERE workspace_id = ? AND proposal_id = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?
        .ok_or_else(not_found)?;
        if state != OPEN {
            return Err(closed());
        }
        let agent: AgentRef = parsed(&agent)?;
        Ok(Open {
            change: parsed(&change)?,
            snapshot: snapshot.as_deref().map(parsed).transpose()?,
            new_id,
            attribution: Attribution::accepted(stored(owner)?, Some(agent), by.clone(), at),
        })
    }

    /// Merges a doc edit into the doc as it is now.
    async fn accept_doc_edit(
        &self,
        transaction: &mut SqliteConnection,
        change: DocChange<'_>,
        attribution: &Attribution,
    ) -> Result<ProposalOutcome, Fault> {
        let at_base = i64::try_from(change.base).map_err(|_| unreadable())?;
        let was = self
            .read_doc(transaction, change.doc, Some(at_base))
            .await?
            .text;
        let current = self.read_doc(transaction, change.doc, None).await?;
        let proposed = doc_text(change.content).map_err(|_| unreadable())?;
        let title = settled(was.title(), current.text.title(), proposed.title());
        let anchors =
            settled(&was.anchors(), &current.text.anchors(), &proposed.anchors()).copied();
        let body = merged(was.body(), current.text.body(), proposed.body()).await?;
        let (Merge::Clean(body), Some(title), Some(anchors)) = (&body, title, anchors) else {
            return Ok(ProposalOutcome::DocConflict {
                hunks: hunks(&body),
                title: title.is_none(),
                anchors: anchors.is_none(),
            });
        };
        let text = match DocText::new(title.clone(), body.clone(), anchors.cloned()) {
            Ok(text) => text,
            Err(DocTextError::TooLarge) => {
                return Err(Fault::new(
                    code!("doc.too_large"),
                    "The merged doc is larger than the limit",
                ));
            }
            // Each side's links matched its pins, but the merged ones don't.
            Err(DocTextError::Unpinned | DocTextError::Unlinked) => {
                return Ok(ProposalOutcome::DocConflict {
                    hunks: vec![MergeHunk::Agreed { text: body.clone() }],
                    title: false,
                    anchors: true,
                });
            }
        };
        let version = current.version;
        let revised = current.revise(version, text).map_err(revise_refused)?;
        self.store_revision(transaction, &revised, attribution)
            .await?;
        Ok(ProposalOutcome::Accepted {
            version: revised.version.get(),
        })
    }

    /// Makes a task edit's edits, unless the task changed since in what they change too.
    async fn accept_task_edit(
        &self,
        transaction: &mut SqliteConnection,
        change: TaskChange<'_>,
        attribution: &Attribution,
    ) -> Result<ProposalOutcome, Fault> {
        let mut task = self.read_task(transaction, change.task).await?;
        let conflicts: Vec<TaskEditBody> = change
            .edits
            .iter()
            .filter(|edit| conflicts(&task, change.snapshot, edit))
            .cloned()
            .collect();
        if !conflicts.is_empty() {
            return Ok(ProposalOutcome::TaskConflict { edits: conflicts });
        }
        for edit in change.edits {
            let version = task.version;
            task = task
                .edit(version, model_edit(edit)?)
                .map_err(edit_refused)?;
            self.store_task(transaction, &task, attribution).await?;
        }
        Ok(ProposalOutcome::Accepted {
            version: task.version.get(),
        })
    }

    /// An order key after every task's, for a new task placed last.
    async fn after_last(&self, transaction: &mut SqliteConnection) -> Result<OrderKey, Fault> {
        let last: Option<String> = sqlx::query_scalar(
            "SELECT order_key FROM tasks WHERE workspace_id = ? ORDER BY order_key DESC LIMIT 1",
        )
        .bind(self.id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        let last: Option<OrderKey> = last.map(stored).transpose()?;
        OrderKey::between(last.as_ref(), None).ok_or_else(no_room)
    }
}

/// A doc edit to accept, written from revision `base`.
struct DocChange<'a> {
    doc: &'a DocId,
    base: u64,
    content: &'a DocContent,
}

/// A task edit to accept: the task's fields when it was proposed, and the edits.
struct TaskChange<'a> {
    task: &'a TaskId,
    snapshot: &'a Snapshot,
    edits: &'a [TaskEditBody],
}

/// What both sides settle on: the proposal's when the doc kept the base's, the doc's when
/// the proposal kept it or made the same, and none when they differ.
fn settled<'a, T: PartialEq + ?Sized>(was: &T, current: &'a T, proposed: &'a T) -> Option<&'a T> {
    if current == was {
        Some(proposed)
    } else if proposed == was || proposed == current {
        Some(current)
    } else {
        None
    }
}

/// The merge of a doc's bodies: one side when only the other kept the base, else the merge,
/// off the async threads; one too large to compare in bounded time is a conflict of the whole
/// body.
async fn merged(was: &str, current: &str, proposed: &str) -> Result<Merge, Fault> {
    if let Some(body) = settled(was, current, proposed) {
        return Ok(Merge::Clean(body.to_owned()));
    }
    if cost(was, current).max(cost(was, proposed)) > MAX_MERGE {
        return Ok(Merge::Conflicted(vec![weacon_model::MergeHunk::Conflict {
            base: was.to_owned(),
            current: current.to_owned(),
            proposed: proposed.to_owned(),
        }]));
    }
    let (was, current, proposed) = (was.to_owned(), current.to_owned(), proposed.to_owned());
    tokio::task::spawn_blocking(move || weacon_model::merge(&was, &current, &proposed))
        .await
        .map_err(|_| Fault::new(code!("proposal.merge_failed"), "The merge failed"))
}

/// The lines times lines aligning `other` with `base` compares, past the start and end they
/// share.
fn cost(base: &str, other: &str) -> usize {
    let (base, other): (Vec<&str>, Vec<&str>) = (
        base.split_inclusive('\n').collect(),
        other.split_inclusive('\n').collect(),
    );
    let start = base
        .iter()
        .zip(&other)
        .take_while(|(one, two)| one == two)
        .count();
    let (base_rest, other_rest) = (base.iter().skip(start), other.iter().skip(start));
    let end = base_rest
        .rev()
        .zip(other_rest.rev())
        .take_while(|(one, two)| one == two)
        .count();
    let middle = |lines: &[&str]| lines.len().saturating_sub(start + end).max(1);
    middle(&base).saturating_mul(middle(&other))
}

/// The hunks of `merge` as the protocol sends them.
fn hunks(merge: &Merge) -> Vec<MergeHunk> {
    match merge {
        Merge::Clean(text) => vec![MergeHunk::Agreed { text: text.clone() }],
        Merge::Conflicted(hunks) => hunks
            .iter()
            .map(|hunk| match hunk {
                weacon_model::MergeHunk::Agreed(text) => MergeHunk::Agreed { text: text.clone() },
                weacon_model::MergeHunk::Conflict {
                    base,
                    current,
                    proposed,
                } => MergeHunk::Conflict {
                    base: base.clone(),
                    current: current.clone(),
                    proposed: proposed.clone(),
                },
            })
            .collect(),
    }
}

/// Whether `edit` changes a field the task has changed in since `snapshot`, to something
/// other than what the edit proposes.
fn conflicts(task: &Task, snapshot: &Snapshot, edit: &TaskEditBody) -> bool {
    match edit {
        TaskEditBody::Title { title } => {
            task.title.as_str() != snapshot.title && task.title.as_str() != title
        }
        TaskEditBody::Status { status } => task.status != snapshot.status && task.status != *status,
        TaskEditBody::Move { .. } | TaskEditBody::Newer => true,
    }
}

/// The model's edit for one a proposal stored.
fn model_edit(edit: &TaskEditBody) -> Result<TaskEdit, Fault> {
    match edit {
        TaskEditBody::Title { title } => Title::try_from(title.clone())
            .map(TaskEdit::Title)
            .map_err(|_| unreadable()),
        TaskEditBody::Status { status } => Ok(TaskEdit::Status(*status)),
        TaskEditBody::Move { .. } | TaskEditBody::Newer => Err(unreadable()),
    }
}

#[cfg(test)]
mod tests {
    use weacon_model::Revision;
    use weacon_protocol::{Author, ProposalState, TaskStatus, WorkspaceChange};
    use weacon_testing::TempDir;

    use super::super::tests::{content, retitle, revise, workspaces};
    use super::*;
    use crate::db::test_support::{NOW, by, code, id};
    use crate::test_support::member_id;
    use crate::test_support::proposals::proposed;

    async fn accepted(workspace: &Workspace, proposal: &str) -> ProposalOutcome {
        workspace
            .accept(&id(proposal), &member_id("m3"), NOW + 5)
            .await
            .unwrap()
    }

    // w1 and w2, with a new doc and a new task proposed in w1, and how many changes w1 had.
    async fn new_things(dir: &TempDir) -> (Workspace, usize) {
        let (one, _) = workspaces(dir).await;
        let new_doc = ProposalChange::NewDoc {
            content: content("Notes", "A\n"),
        };
        let new_task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        one.propose(&proposed("p1", "mc1", new_doc)).await.unwrap();
        one.propose(&proposed("p2", "mc1", new_task)).await.unwrap();
        let seen = one.changes_since(0, 100).await.unwrap().len();
        (one, seen)
    }

    #[tokio::test]
    async fn a_new_doc_is_made_with_the_agent_as_author_and_the_person_who_accepted() {
        let dir = TempDir::new().unwrap();
        let (one, _) = new_things(&dir).await;

        let outcome = accepted(&one, "p1").await;
        assert_eq!(outcome, ProposalOutcome::Accepted { version: 1 });
        let doc = one.doc(&id("new-p1"), None).await.unwrap();
        assert_eq!(doc.text.body(), "A\n");
        let history = one.doc_history(&id("new-p1"), 5).await.unwrap();
        let agent = proposed("p1", "mc1", ProposalChange::Newer).agent;
        let author = Author::Agent {
            owner: member_id("m1"),
            agent: Some(agent),
        };
        assert_eq!(history[0].author, author);
        assert_eq!(
            (&history[0].accepted_by, history[0].at),
            (&member_id("m3"), NOW + 5)
        );
        let again = one.accept(&id("p1"), &member_id("m3"), NOW).await;
        assert_eq!(code(&again.unwrap_err()), "proposal.closed");
    }

    #[tokio::test]
    async fn a_new_task_goes_last_and_the_feed_hears_of_each_change() {
        let dir = TempDir::new().unwrap();
        let (one, seen) = new_things(&dir).await;

        accepted(&one, "p1").await;
        let outcome = accepted(&one, "p2").await;
        assert_eq!(outcome, ProposalOutcome::Accepted { version: 1 });
        let tasks = one.tasks().await.unwrap();
        let titles: Vec<&str> = tasks.iter().map(|task| task.title.as_str()).collect();
        assert_eq!(titles, ["Write", "Ship"]);
        let record = one.proposal(&id("p2")).await.unwrap();
        let by = member_id("m3");
        assert_eq!(record.state, ProposalState::Accepted { version: 1, by });
        assert_eq!(record.change, None);
        let changes = one.changes_since(0, 100).await.unwrap();
        let heard: Vec<_> = changes
            .into_iter()
            .skip(seen)
            .map(|update| update.change)
            .collect();
        let (doc, task) = (id("new-p1"), id("new-p2"));
        assert_eq!(
            heard,
            [
                WorkspaceChange::DocChanged { doc, version: 1 },
                WorkspaceChange::ProposalChanged { proposal: id("p1") },
                WorkspaceChange::TaskChanged { task, version: 1 },
                WorkspaceChange::ProposalChanged { proposal: id("p2") },
            ]
        );
    }

    async fn revised_by_a_person(workspace: &Workspace, body: &str) {
        let text = DocText::new(id("Plan"), body.to_owned(), None).unwrap();
        workspace
            .revise_doc(&id("d1"), Revision::FIRST, text, &by("m3"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_doc_edit_merges_with_what_changed_since_its_base() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        one.propose(&proposed(
            "p1",
            "mc1",
            revise("d1", 1, "One\nTwo\nThree, done\n"),
        ))
        .await
        .unwrap();
        revised_by_a_person(&one, "Zero\nOne\nTwo\nThree\n").await;

        assert_eq!(
            accepted(&one, "p1").await,
            ProposalOutcome::Accepted { version: 3 }
        );
        let doc = one.doc(&id("d1"), None).await.unwrap();
        assert_eq!(doc.text.body(), "Zero\nOne\nTwo\nThree, done\n");
    }

    #[tokio::test]
    async fn a_conflict_is_answered_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let mut retitled = revise("d1", 1, "One\nTwo, theirs\nThree\n");
        if let ProposalChange::DocEdit { content, .. } = &mut retitled {
            content.title = "Plan B".to_owned();
        }
        one.propose(&proposed("p1", "mc1", retitled)).await.unwrap();
        revised_by_a_person(&one, "One\nTwo, mine\nThree\n").await;
        let seen = one.changes_since(0, 100).await.unwrap().len();

        let outcome = accepted(&one, "p1").await;
        assert_eq!(
            outcome,
            ProposalOutcome::DocConflict {
                hunks: vec![
                    MergeHunk::Agreed {
                        text: "One\n".to_owned()
                    },
                    MergeHunk::Conflict {
                        base: "Two\n".to_owned(),
                        current: "Two, mine\n".to_owned(),
                        proposed: "Two, theirs\n".to_owned(),
                    },
                    MergeHunk::Agreed {
                        text: "Three\n".to_owned()
                    },
                ],
                title: false,
                anchors: false,
            }
        );
        assert_eq!(
            one.doc(&id("d1"), None).await.unwrap().version,
            Revision::new(2)
        );
        assert_eq!(
            one.proposal(&id("p1")).await.unwrap().state,
            ProposalState::Open
        );
        assert_eq!(one.changes_since(0, 100).await.unwrap().len(), seen);
    }

    #[tokio::test]
    async fn titles_and_anchors_both_sides_changed_are_conflicts() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let mut retitled = revise("d1", 1, "One\nTwo\nThree\n");
        if let ProposalChange::DocEdit { content, .. } = &mut retitled {
            content.title = "Plan B".to_owned();
        }
        one.propose(&proposed("p1", "mc1", retitled)).await.unwrap();
        let text = DocText::new(id("Plan C"), "One\nTwo\nThree\n".to_owned(), None).unwrap();
        one.revise_doc(&id("d1"), Revision::FIRST, text, &by("m3"))
            .await
            .unwrap();

        let ProposalOutcome::DocConflict { title, anchors, .. } = accepted(&one, "p1").await else {
            panic!("no conflict");
        };
        assert!(title && !anchors);
    }

    #[tokio::test]
    async fn a_task_edit_lands_unless_the_task_changed_since_in_the_same_field() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let title = || TaskEditBody::Title {
            title: "Write it".to_owned(),
        };
        let done = || TaskEditBody::Status {
            status: TaskStatus::Done,
        };
        let task = id("t1");
        let both = ProposalChange::TaskEdit {
            task: id("t1"),
            base: 1,
            edits: vec![title(), done()],
        };
        one.propose(&proposed("p1", "mc1", both)).await.unwrap();
        let other = proposed("p2", "mc2", retitle("t1", 1, "Write all"));
        one.propose(&other).await.unwrap();
        let started = TaskEdit::Status(TaskStatus::InProgress);
        one.edit_task(&task, Revision::FIRST, started, &by("m3"))
            .await
            .unwrap();

        let conflict = accepted(&one, "p1").await;
        assert_eq!(
            conflict,
            ProposalOutcome::TaskConflict {
                edits: vec![done()]
            }
        );
        let outcome = accepted(&one, "p2").await;
        assert_eq!(outcome, ProposalOutcome::Accepted { version: 3 });
        let task = &one.tasks().await.unwrap()[0];
        let fields = (task.title.as_str(), task.status);
        assert_eq!(fields, ("Write all", TaskStatus::InProgress));
        let renamed = accepted(&one, "p1").await;
        let edits = vec![title(), done()];
        assert_eq!(renamed, ProposalOutcome::TaskConflict { edits });
    }

    #[tokio::test]
    async fn a_proposal_there_isnt_is_not_found() {
        let dir = TempDir::new().unwrap();
        let (one, two) = workspaces(&dir).await;
        one.propose(&proposed("p1", "mc1", revise("d1", 1, "A\n")))
            .await
            .unwrap();

        let missing = two
            .accept(&id("p1"), &member_id("m2"), NOW)
            .await
            .unwrap_err();
        assert_eq!(code(&missing), "proposal.not_found");
    }

    #[test]
    fn a_side_that_kept_the_base_takes_the_other() {
        assert_eq!(settled("a", "a", "b"), Some("b"));
        assert_eq!(settled("a", "b", "a"), Some("b"));
        assert_eq!(settled("a", "b", "b"), Some("b"));
        assert_eq!(settled("a", "b", "c"), None);
    }

    #[tokio::test]
    async fn a_new_task_goes_after_every_other() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        for (task, title, order) in [("t2", "Test", "b"), ("t3", "Plan", "c")] {
            let task = crate::test_support::new_task(task, title, order);
            one.add_task(&task, &by("m1")).await.unwrap();
        }
        let new_task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        one.propose(&proposed("p1", "mc1", new_task)).await.unwrap();

        accepted(&one, "p1").await;
        let tasks = one.tasks().await.unwrap();
        let titles: Vec<&str> = tasks.iter().map(|task| task.title.as_str()).collect();
        assert_eq!(titles, ["Write", "Test", "Plan", "Ship"]);
    }

    #[tokio::test]
    async fn a_proposal_a_full_workspace_holds_can_still_be_accepted() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        one.propose(&proposed("p1", "mc1", revise("d1", 1, "One\nTwo\n")))
            .await
            .unwrap();
        one.fill_doc_quota().await;

        assert_eq!(
            accepted(&one, "p1").await,
            ProposalOutcome::Accepted { version: 2 }
        );
    }

    #[tokio::test]
    async fn a_merge_past_the_doc_limit_is_refused_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let big = "x".repeat(130 * 1024);
        let ours = format!("{big}\nOne\nTwo\nThree\n");
        let theirs = format!("One\nTwo\nThree\n{big}\n");
        one.propose(&proposed("p1", "mc1", revise("d1", 1, &theirs)))
            .await
            .unwrap();
        revised_by_a_person(&one, &ours).await;

        let fault = one
            .accept(&id("p1"), &member_id("m3"), NOW)
            .await
            .unwrap_err();
        assert_eq!(code(&fault), "doc.too_large");
        assert_eq!(
            one.proposal(&id("p1")).await.unwrap().state,
            ProposalState::Open
        );
    }

    // A body of `lines` lines between a first and last line that stay, each made of `side`.
    fn rewritten(side: &str, lines: usize) -> String {
        let middle: String = (0..lines)
            .flat_map(|line| [side.to_owned(), line.to_string(), "\n".to_owned()])
            .collect();
        format!("first\n{middle}last\n")
    }

    #[tokio::test]
    async fn a_merge_only_compares_so_many_rewritten_lines() {
        let side = MAX_MERGE.isqrt();
        let mut outcomes = Vec::new();
        for lines in [side, side + 1] {
            let (was, current, proposed) = (
                rewritten("b", lines),
                rewritten("c", lines),
                rewritten("p", lines),
            );
            outcomes.push(merged(&was, &current, &proposed).await.unwrap());
        }

        let Merge::Conflicted(compared) = &outcomes[0] else {
            panic!("{:?}", outcomes[0]);
        };
        assert_eq!(compared.len(), 3);
        let Merge::Conflicted(whole) = &outcomes[1] else {
            panic!("{:?}", outcomes[1]);
        };
        assert!(matches!(
            whole.as_slice(),
            [weacon_model::MergeHunk::Conflict { .. }]
        ));
    }

    #[tokio::test]
    async fn a_long_doc_with_small_changes_on_both_sides_merges() {
        let lines: String = (0..5_000)
            .flat_map(|line| ["line ".to_owned(), line.to_string(), "\n".to_owned()])
            .collect();
        let current = lines.replacen("line 10\n", "line ten\n", 1);
        let proposed = lines.replacen("line 4990\n", "line 4990, done\n", 1);

        let Merge::Clean(body) = merged(&lines, &current, &proposed).await.unwrap() else {
            panic!("conflict");
        };
        assert!(body.contains("line ten\n") && body.contains("line 4990, done\n"));
        assert_eq!(cost(&lines, &lines), 1);
        assert_eq!(cost("a\nb\nc\n", "a\nx\ny\nc\n"), 2);
    }
}
