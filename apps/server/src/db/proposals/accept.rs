use std::ops::ControlFlow::{self, Break, Continue};

use sqlx::pool::PoolConnection;
use sqlx::{Sqlite, SqliteConnection};
use weacon_model::{Attribution, Doc, DocText, Merged, Revision, Task, TaskEdit, Title};
use weacon_observability::Fault;
use weacon_protocol::{
    AgentRef, DocId, MemberId, ProposalChange, ProposalId, ProposalOutcome, TaskEditBody, TaskId,
    code,
};

use super::super::docs::refused as revise_refused;
use super::super::tasks::refused as edit_refused;
use super::super::{Workspace, failed, parsed, stored, unreadable, version_number};
use super::target::{Snapshot, doc_text, proposable};
use super::{Closing, OPEN, closed, not_found};

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
    change: Change,
    attribution: Attribution,
}

/// A proposal's change, checked as it is read.
enum Change {
    DocEdit {
        doc: DocId,
        base: Revision,
        text: DocText,
    },
    TaskEdit {
        task: TaskId,
        edits: Vec<(TaskEditBody, TaskEdit)>,
        was: Snapshot,
    },
    NewDoc(Doc),
    NewTask {
        id: TaskId,
        title: Title,
    },
}

/// A change ready to make once the workspace is locked: a doc edit is merged already, into
/// the doc at revision `read_at`.
enum Ready {
    Revision {
        doc: DocId,
        read_at: Revision,
        text: DocText,
    },
    TaskEdit {
        task: TaskId,
        edits: Vec<(TaskEditBody, TaskEdit)>,
        was: Snapshot,
    },
    NewDoc(Doc),
    NewTask {
        id: TaskId,
        title: Title,
    },
}

/// What accepting makes, before it is stored.
enum Made {
    Revision(Doc),
    /// Each version a task edit's edits make, in order.
    Task(Vec<Task>),
    NewDoc(Doc),
    NewTask(Task),
}

impl Workspace {
    /// Accepts the open proposal `id` on behalf of `by`, at `at`: the doc or task changes as
    /// it proposes and keeps its author, and the proposal closes. A conflict changes nothing,
    /// and so does a doc revised while its edit was merged, which is `doc.stale`.
    pub(crate) async fn accept(
        &self,
        id: &ProposalId,
        by: &MemberId,
        at: i64,
    ) -> Result<ProposalOutcome, Fault> {
        // A doc edit is merged before the workspace is locked, so that a long merge holds
        // up no other write.
        let mut reader = self.db.readers.acquire().await.map_err(failed)?;
        let Open {
            change,
            attribution,
        } = self.open_proposal(&mut reader, id, by, at).await?;
        let ready = match self.ready(reader, change).await? {
            Continue(ready) => ready,
            Break(conflict) => return Ok(conflict),
        };
        let mut transaction = self.db.write().await?;
        self.open_bytes(&mut transaction, id).await?;
        let made = match self.apply_change(&mut transaction, ready).await? {
            Continue(made) => made,
            Break(conflict) => return Ok(conflict),
        };
        let version = made.version().ok_or_else(unreadable)?;
        let accepted = Closing::Accepted {
            by,
            version: version_number(version, unreadable)?,
        };
        // Its own bytes leave the quota first, so that a full workspace can still accept it.
        self.close_proposal(&mut transaction, id, accepted, by)
            .await?;
        self.store_made(&mut transaction, &made, &attribution)
            .await?;
        transaction.commit().await.map_err(failed)?;
        Ok(ProposalOutcome::Accepted {
            version: version.get(),
        })
    }

    /// The open proposal `id`, with the attribution its change takes once `by` accepts it;
    /// `proposal.not_found` when there's none, and `proposal.closed` when it isn't open.
    async fn open_proposal(
        &self,
        connection: &mut SqliteConnection,
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
        .fetch_optional(&mut *connection)
        .await
        .map_err(failed)?
        .ok_or_else(not_found)?;
        if state != OPEN {
            return Err(closed());
        }
        let agent: AgentRef = parsed(&agent)?;
        let snapshot = snapshot.as_deref().map(parsed).transpose()?;
        Ok(Open {
            change: checked(parsed(&change)?, snapshot, new_id).ok_or_else(unreadable)?,
            attribution: Attribution::accepted(stored(owner)?, Some(agent), by.clone(), at),
        })
    }

    /// `change`, with a doc edit merged into the doc as it is now, or what conflicts. The
    /// reader goes back to the pool before the merge, which takes a while.
    async fn ready(
        &self,
        mut reader: PoolConnection<Sqlite>,
        change: Change,
    ) -> Result<ControlFlow<ProposalOutcome, Ready>, Fault> {
        let ready = match change {
            Change::DocEdit { doc, base, text } => {
                let was = self.read_doc(&mut reader, &doc, Some(base)).await?.text;
                let current = self.read_doc(&mut reader, &doc, None).await?;
                drop(reader);
                let read_at = current.version;
                match merged(was, current.text, text).await? {
                    Merged::Text(text) => Ready::Revision { doc, read_at, text },
                    Merged::Conflict {
                        hunks,
                        title,
                        anchors,
                    } => {
                        let conflict = ProposalOutcome::DocConflict {
                            hunks,
                            title,
                            anchors,
                        };
                        return Ok(Break(conflict));
                    }
                }
            }
            Change::TaskEdit { task, edits, was } => Ready::TaskEdit { task, edits, was },
            Change::NewDoc(doc) => Ready::NewDoc(doc),
            Change::NewTask { id, title } => Ready::NewTask { id, title },
        };
        Ok(Continue(ready))
    }

    /// What `ready` makes, read and checked inside the caller's transaction, or what
    /// conflicts with it.
    async fn apply_change(
        &self,
        transaction: &mut SqliteConnection,
        ready: Ready,
    ) -> Result<ControlFlow<ProposalOutcome, Made>, Fault> {
        let made = match ready {
            Ready::Revision { doc, read_at, text } => {
                let now = self.read_doc(transaction, &doc, None).await?;
                Made::Revision(now.revise(read_at, text).map_err(revise_refused)?)
            }
            Ready::TaskEdit { task, edits, was } => {
                let task = self.read_task(transaction, &task).await?;
                return edited(task, &was, edits);
            }
            Ready::NewDoc(doc) => Made::NewDoc(doc),
            Ready::NewTask { id, title } => {
                let order = self.after_last(transaction).await?;
                Made::NewTask(Task::new(id, title, order))
            }
        };
        Ok(Continue(made))
    }

    async fn store_made(
        &self,
        transaction: &mut SqliteConnection,
        made: &Made,
        attribution: &Attribution,
    ) -> Result<(), Fault> {
        match made {
            Made::Revision(doc) => self.store_revision(transaction, doc, attribution).await,
            Made::NewDoc(doc) => self.insert_doc(transaction, doc, attribution).await,
            Made::NewTask(task) => self.insert_task(transaction, task, attribution).await,
            Made::Task(versions) => {
                for task in versions {
                    self.store_task(transaction, task, attribution).await?;
                }
                Ok(())
            }
        }
    }
}

impl Made {
    fn version(&self) -> Option<Revision> {
        match self {
            Self::Revision(doc) | Self::NewDoc(doc) => Some(doc.version),
            Self::NewTask(task) => Some(task.version),
            Self::Task(versions) => versions.last().map(|task| task.version),
        }
    }
}

/// A stored change as accepting it needs it; `None` for one this server never stores.
fn checked(
    change: ProposalChange,
    snapshot: Option<Snapshot>,
    new_id: Option<String>,
) -> Option<Change> {
    Some(match change {
        ProposalChange::DocEdit { doc, base, content } => Change::DocEdit {
            doc,
            base: Revision::new(base),
            text: doc_text(&content).ok()?,
        },
        ProposalChange::TaskEdit { task, edits, .. } => Change::TaskEdit {
            task,
            edits: edits
                .into_iter()
                .map(|edit| Some((edit.clone(), proposable(edit)?)))
                .collect::<Option<_>>()?,
            was: snapshot?,
        },
        ProposalChange::NewDoc { content } => {
            let id = DocId::try_from(new_id?).ok()?;
            Change::NewDoc(Doc::new(id, doc_text(&content).ok()?))
        }
        ProposalChange::NewTask { title } => Change::NewTask {
            id: TaskId::try_from(new_id?).ok()?,
            title: Title::try_from(title).ok()?,
        },
        ProposalChange::Newer => return None,
    })
}

/// Each version `edits` make of `task`, unless it changed since `was` in what they change too.
fn edited(
    mut task: Task,
    was: &Snapshot,
    edits: Vec<(TaskEditBody, TaskEdit)>,
) -> Result<ControlFlow<ProposalOutcome, Made>, Fault> {
    let conflicts: Vec<TaskEditBody> = edits
        .iter()
        .filter(|(_, edit)| conflicts(&task, was, edit))
        .map(|(body, _)| body.clone())
        .collect();
    if !conflicts.is_empty() {
        return Ok(Break(ProposalOutcome::TaskConflict { edits: conflicts }));
    }
    let mut versions = Vec::new();
    for (_, edit) in edits {
        let version = task.version;
        task = task.edit(version, edit).map_err(edit_refused)?;
        versions.push(task.clone());
    }
    Ok(Continue(Made::Task(versions)))
}

/// Whether `edit` changes a field the task has changed in since `snapshot`, to something
/// other than what the edit proposes.
fn conflicts(task: &Task, snapshot: &Snapshot, edit: &TaskEdit) -> bool {
    match edit {
        TaskEdit::Title(title) => task.title.as_str() != snapshot.title && task.title != *title,
        TaskEdit::Status(status) => task.status != snapshot.status && task.status != *status,
        TaskEdit::Order(_) => true,
    }
}

/// The merge of a doc edit, off the async threads.
async fn merged(was: DocText, current: DocText, proposed: DocText) -> Result<Merged, Fault> {
    tokio::task::spawn_blocking(move || DocText::merge(&was, &current, &proposed, MAX_MERGE))
        .await
        .map_err(|_| Fault::new(code!("proposal.merge_failed"), "The merge failed"))?
        .map_err(|_| too_large())
}

fn too_large() -> Fault {
    Fault::new(
        code!("doc.too_large"),
        "The merged doc is larger than the limit",
    )
}

#[cfg(test)]
mod tests {
    use weacon_model::Revision;
    use weacon_protocol::{
        Author, CommitId, DocAnchors, DocPin, MergeHunk, ProposalState, TaskStatus, WorkspaceChange,
    };
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
                WorkspaceChange::ProposalChanged { proposal: id("p1") },
                WorkspaceChange::DocChanged { doc, version: 1 },
                WorkspaceChange::ProposalChanged { proposal: id("p2") },
                WorkspaceChange::TaskChanged { task, version: 1 },
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

    #[tokio::test]
    async fn an_edit_lands_when_the_task_changed_only_elsewhere_or_the_same_way() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let done = |task: &str| ProposalChange::TaskEdit {
            task: id(task),
            base: 1,
            edits: vec![TaskEditBody::Status {
                status: TaskStatus::Done,
            }],
        };
        one.add_task(&crate::test_support::new_task("t2", "Test", "b"), &by("m1"))
            .await
            .unwrap();
        one.propose(&proposed("p1", "mc1", done("t1")))
            .await
            .unwrap();
        one.propose(&proposed("p2", "mc1", done("t2")))
            .await
            .unwrap();
        let renamed = TaskEdit::Title(id("Write more"));
        one.edit_task(&id("t1"), Revision::FIRST, renamed, &by("m3"))
            .await
            .unwrap();
        let finished = TaskEdit::Status(TaskStatus::Done);
        one.edit_task(&id("t2"), Revision::FIRST, finished, &by("m3"))
            .await
            .unwrap();

        assert_eq!(
            accepted(&one, "p1").await,
            ProposalOutcome::Accepted { version: 3 }
        );
        assert_eq!(
            accepted(&one, "p2").await,
            ProposalOutcome::Accepted { version: 3 }
        );
    }

    #[tokio::test]
    async fn a_doc_edit_that_links_to_code_lands_with_its_pin() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let anchors = DocAnchors {
            commit: CommitId::try_from("a".repeat(40)).unwrap(),
            pins: vec![DocPin::try_from("sym:a#0123456789abcdef".to_owned()).unwrap()],
        };
        let mut linked = revise("d1", 1, "See [a](sym:a).\nTwo\nThree\n");
        if let ProposalChange::DocEdit { content, .. } = &mut linked {
            content.anchors = Some(anchors.clone());
        }
        one.propose(&proposed("p1", "mc1", linked)).await.unwrap();
        revised_by_a_person(&one, "One\nTwo\nThree\nFour\n").await;

        assert_eq!(
            accepted(&one, "p1").await,
            ProposalOutcome::Accepted { version: 3 }
        );
        let doc = one.doc(&id("d1"), None).await.unwrap();
        assert_eq!(doc.text.body(), "See [a](sym:a).\nTwo\nThree\nFour\n");
        assert_eq!(doc.text.anchors(), Some(&anchors));
    }

    #[tokio::test]
    async fn a_doc_revised_while_its_edit_merged_is_stale_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let edit = revise("d1", 1, "One\nTwo\nThree\nFour\n");
        one.propose(&proposed("p1", "mc1", edit)).await.unwrap();
        let mut reader = one.db.readers.acquire().await.unwrap();
        let open = one
            .open_proposal(&mut reader, &id("p1"), &member_id("m3"), NOW)
            .await
            .unwrap();
        let Ok(Continue(ready)) = one.ready(reader, open.change).await else {
            panic!("not merged");
        };
        revised_by_a_person(&one, "Zero\nOne\nTwo\nThree\n").await;

        let mut transaction = one.db.write().await.unwrap();
        let Err(fault) = one.apply_change(&mut transaction, ready).await else {
            panic!("not refused");
        };
        drop(transaction);
        assert_eq!(code(&fault), "doc.stale");
        assert_eq!(
            one.proposal(&id("p1")).await.unwrap().state,
            ProposalState::Open
        );
    }
}
