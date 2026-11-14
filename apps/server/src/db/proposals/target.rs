use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use weacon_model::{DocText, Revision, TaskEdit, Title};
use weacon_observability::Fault;
use weacon_protocol::{
    Code, DocContent, DocId, ProposalChange, TaskEditBody, TaskId, TaskStatus, code,
};

use super::super::{Workspace, version_number};
use super::{DOC_EDIT, NEW_DOC, NEW_TASK, Proposed, TASK_EDIT, invalid};

/// What a change is to, once checked against what the workspace holds now.
#[derive(Default)]
pub(super) struct Target<'a> {
    pub(super) kind: &'static str,
    pub(super) doc: Option<&'a DocId>,
    pub(super) task: Option<&'a TaskId>,
    pub(super) new_id: Option<&'a str>,
    pub(super) base: Option<i64>,
    /// A task's [`Snapshot`], as JSON.
    pub(super) snapshot: Option<String>,
}

/// A task's fields as an edit found them, to tell later what changed since.
#[derive(Serialize, Deserialize)]
pub(super) struct Snapshot {
    pub(super) title: String,
    pub(super) status: TaskStatus,
}

impl Workspace {
    /// Checks `proposed`'s change against the workspace as it is now.
    pub(super) async fn proposal_target<'a>(
        &self,
        transaction: &mut SqliteConnection,
        proposed: &'a Proposed,
    ) -> Result<Target<'a>, Fault> {
        let new_id = Some(proposed.fresh.as_str());
        match &proposed.change {
            ProposalChange::DocEdit { doc, base, content } => {
                let text = doc_text(content)?;
                let base = Revision::new(*base);
                // A revision there isn't, and an edit that changes nothing, are both refused.
                let was = self.read_doc(transaction, doc, Some(base)).await;
                let was = was.map_err(|fault| invalid_if(fault, &code!("doc.not_found")))?;
                if was.text == text {
                    return Err(invalid());
                }
                Ok(Target {
                    kind: DOC_EDIT,
                    doc: Some(doc),
                    base: Some(version_number(base, invalid)?),
                    ..Target::default()
                })
            }
            ProposalChange::TaskEdit { task, base, edits } => {
                self.task_target(transaction, task, *base, edits).await
            }
            ProposalChange::NewDoc { content } => {
                doc_text(content)?;
                DocId::try_from(proposed.fresh.clone()).map_err(|_| invalid())?;
                Ok(Target {
                    kind: NEW_DOC,
                    new_id,
                    ..Target::default()
                })
            }
            ProposalChange::NewTask { title } => {
                Title::try_from(title.clone()).map_err(|_| invalid())?;
                TaskId::try_from(proposed.fresh.clone()).map_err(|_| invalid())?;
                Ok(Target {
                    kind: NEW_TASK,
                    new_id,
                    ..Target::default()
                })
            }
            ProposalChange::Newer => Err(invalid()),
        }
    }

    /// A task edit's target, made only from the task's version as it is now;
    /// `proposal.stale` otherwise.
    async fn task_target<'a>(
        &self,
        transaction: &mut SqliteConnection,
        task: &'a TaskId,
        base: u64,
        edits: &[TaskEditBody],
    ) -> Result<Target<'a>, Fault> {
        let found = self
            .read_task(transaction, task)
            .await
            .map_err(|fault| invalid_if(fault, &code!("task.not_found")))?;
        if found.version.get() != base {
            return Err(Fault::new(
                code!("proposal.stale"),
                "The task has changed since the proposal was made",
            ));
        }
        let snapshot = Snapshot {
            title: found.title.as_str().to_owned(),
            status: found.status,
        };
        task_edits(edits, &snapshot)?;
        Ok(Target {
            kind: TASK_EDIT,
            task: Some(task),
            base: Some(version_number(Revision::new(base), invalid)?),
            snapshot: Some(serde_json::to_string(&snapshot).map_err(|_| invalid())?),
            ..Target::default()
        })
    }
}

/// `proposal.invalid` for a doc or task there isn't, whose code is `missing`; any other
/// fault stays itself.
fn invalid_if(fault: Fault, missing: &Code) -> Fault {
    if fault.code() == missing {
        invalid()
    } else {
        fault
    }
}

/// The doc text `content` holds, or `proposal.invalid`.
pub(super) fn doc_text(content: &DocContent) -> Result<DocText, Fault> {
    let title = Title::try_from(content.title.clone()).map_err(|_| invalid())?;
    let text = DocText::new(title, content.body.clone(), content.anchors.clone());
    text.map_err(|_| invalid())
}

/// The model's edit for one a proposal may carry: a title or a status this build knows.
pub(super) fn proposable(edit: TaskEditBody) -> Option<TaskEdit> {
    match edit {
        TaskEditBody::Title { title } => Title::try_from(title).ok().map(TaskEdit::Title),
        TaskEditBody::Status { status } if status != TaskStatus::Newer => {
            Some(TaskEdit::Status(status))
        }
        TaskEditBody::Status { .. } | TaskEditBody::Move { .. } | TaskEditBody::Newer => None,
    }
}

/// Refuses edits a proposal can't make: none at all, one it may not carry, or one that
/// changes nothing about the task as it is.
fn task_edits(edits: &[TaskEditBody], task: &Snapshot) -> Result<(), Fault> {
    let changes = |edit: TaskEdit| match edit {
        TaskEdit::Title(title) => title.as_str() != task.title,
        TaskEdit::Status(status) => status != task.status,
        TaskEdit::Order(_) => false,
    };
    let fine = |edit: &TaskEditBody| proposable(edit.clone()).is_some_and(changes);
    if edits.is_empty() || !edits.iter().all(fine) {
        return Err(invalid());
    }
    Ok(())
}
