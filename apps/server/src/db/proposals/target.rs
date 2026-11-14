use sqlx::SqliteConnection;
use weacon_model::{DocText, Title};
use weacon_observability::Fault;
use weacon_protocol::{DocContent, DocId, ProposalChange, TaskEditBody, TaskId, TaskStatus, code};

use super::super::Workspace;
use super::{Proposed, invalid};

/// What a change is to, once checked against what the workspace holds now.
#[derive(Default)]
pub(super) struct Target<'a> {
    pub(super) kind: &'static str,
    pub(super) doc: Option<&'a DocId>,
    pub(super) task: Option<&'a TaskId>,
    pub(super) new_id: Option<&'a str>,
    pub(super) base: Option<i64>,
    /// A task's fields as the edit found them, to tell later what changed since.
    pub(super) snapshot: Option<String>,
}

impl Workspace {
    /// Checks `proposed`'s change against the workspace as it is now.
    pub(super) async fn target<'a>(
        &self,
        transaction: &mut SqliteConnection,
        proposed: &'a Proposed,
    ) -> Result<Target<'a>, Fault> {
        let fresh = Some(proposed.fresh.as_str());
        match &proposed.change {
            ProposalChange::DocEdit { doc, base, content } => {
                doc_text(content)?;
                let current = self.read_doc(transaction, doc, None).await;
                let current = current.map_err(|_| invalid())?.version.get();
                if *base == 0 || *base > current {
                    return Err(invalid());
                }
                Ok(Target {
                    kind: "doc_edit",
                    doc: Some(doc),
                    base: Some(i64::try_from(*base).map_err(|_| invalid())?),
                    ..Target::default()
                })
            }
            ProposalChange::TaskEdit { task, base, edits } => {
                self.task_target(transaction, task, *base, edits).await
            }
            ProposalChange::NewDoc { content } => {
                doc_text(content)?;
                DocId::try_from(proposed.fresh.clone()).map_err(|_| invalid())?;
                let kind = "new_doc";
                Ok(Target {
                    kind,
                    new_id: fresh,
                    ..Target::default()
                })
            }
            ProposalChange::NewTask { title } => {
                Title::try_from(title.clone()).map_err(|_| invalid())?;
                TaskId::try_from(proposed.fresh.clone()).map_err(|_| invalid())?;
                let kind = "new_task";
                Ok(Target {
                    kind,
                    new_id: fresh,
                    ..Target::default()
                })
            }
            ProposalChange::Newer => Err(invalid()),
        }
    }

    /// A task edit's target: only one made from the task's version as it is now, as only that
    /// version's fields are known, which it keeps to tell later what changed since.
    async fn task_target<'a>(
        &self,
        transaction: &mut SqliteConnection,
        task: &'a TaskId,
        base: u64,
        edits: &[TaskEditBody],
    ) -> Result<Target<'a>, Fault> {
        let found = self.read_task(transaction, task).await;
        let found = found.map_err(|_| invalid())?;
        if found.version.get() != base {
            return Err(Fault::new(
                code!("proposal.stale"),
                "The task has changed since the proposal was made",
            ));
        }
        task_edits(edits)?;
        let snapshot = [
            TaskEditBody::Title {
                title: found.title.as_str().to_owned(),
            },
            TaskEditBody::Status {
                status: found.status,
            },
        ];
        Ok(Target {
            kind: "task_edit",
            task: Some(task),
            base: Some(i64::try_from(base).map_err(|_| invalid())?),
            snapshot: Some(serde_json::to_string(&snapshot).map_err(|_| invalid())?),
            ..Target::default()
        })
    }
}

/// The doc text `content` holds, or `proposal.invalid`.
pub(super) fn doc_text(content: &DocContent) -> Result<DocText, Fault> {
    let title = Title::try_from(content.title.clone()).map_err(|_| invalid())?;
    let text = DocText::new(title, content.body.clone(), content.anchors.clone());
    text.map_err(|_| invalid())
}

/// Refuses edits a proposal can't make: none at all, a move, or a title the model refuses.
fn task_edits(edits: &[TaskEditBody]) -> Result<(), Fault> {
    let fine = |edit: &TaskEditBody| match edit {
        TaskEditBody::Title { title } => Title::try_from(title.clone()).is_ok(),
        TaskEditBody::Status { status } => *status != TaskStatus::Newer,
        TaskEditBody::Move { .. } | TaskEditBody::Newer => false,
    };
    if edits.is_empty() || !edits.iter().all(fine) {
        return Err(invalid());
    }
    Ok(())
}
