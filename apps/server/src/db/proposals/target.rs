use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;
use weacon_model::{DocText, Revision, TaskEdit, Title, reanchored};
use weacon_observability::Fault;
use weacon_protocol::{
    AnchorChange, Code, DocContent, DocId, MachineId, ProposalChange, StaleAnchor, TaskEditBody,
    TaskId, TaskStatus, code,
};

use super::super::{Workspace, version_number};
use super::{DOC_EDIT, NEEDS_UPDATE, NEW_DOC, NEW_TASK, Proposed, TASK_EDIT, invalid};

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
                self.edit_target(transaction, proposed, (doc, *base), content)
                    .await
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
            ProposalChange::NeedsUpdate { doc, base, anchors } => {
                let machine = &proposed.machine;
                self.stale_target(transaction, (doc, *base), anchors, machine)
                    .await
            }
            ProposalChange::Newer => Err(invalid()),
        }
    }

    /// A doc edit's target, made from the doc's revision `base`; what a core proposes of its
    /// own only re-anchors pins of that revision or of its own open re-anchor from there.
    async fn edit_target<'a>(
        &self,
        transaction: &mut SqliteConnection,
        proposed: &Proposed,
        (doc, base): (&'a DocId, u64),
        content: &DocContent,
    ) -> Result<Target<'a>, Fault> {
        let text = doc_text(content)?;
        // A revision there isn't, and an edit that changes nothing, are both refused.
        let was = self
            .read_doc(transaction, doc, Some(Revision::new(base)))
            .await;
        let was = was.map_err(|fault| invalid_if(fault, &code!("doc.not_found")))?;
        if was.text == text {
            return Err(invalid());
        }
        if proposed.agent.is_none() && !reanchors(&DocContent::from(was.text), content) {
            let own = self.own_change(transaction, doc, base, &proposed.machine);
            if !own.await?.is_some_and(|own| reanchors(&own, content)) {
                return Err(invalid());
            }
        }
        Ok(Target {
            kind: DOC_EDIT,
            doc: Some(doc),
            base: Some(version_number(Revision::new(base), invalid)?),
            ..Target::default()
        })
    }

    /// A "needs an update" proposal's target: anchors that the doc pins at its revision `base`,
    /// or that `machine`'s own open re-anchor from there would pin.
    async fn stale_target<'a>(
        &self,
        transaction: &mut SqliteConnection,
        (doc, base): (&'a DocId, u64),
        anchors: &[StaleAnchor],
        machine: &MachineId,
    ) -> Result<Target<'a>, Fault> {
        let own = self.own_change(transaction, doc, base, machine).await?;
        let own = own.and_then(|own| own.anchors).map(|own| own.pins);
        let base = Revision::new(base);
        let was = self.read_doc(transaction, doc, Some(base)).await;
        let was = was.map_err(|fault| invalid_if(fault, &code!("doc.not_found")))?;
        let pins = was.text.anchors().map_or(&[][..], |anchors| &anchors.pins);
        let own = own.unwrap_or_default();
        let pinned = |stale: &StaleAnchor| pins.contains(&stale.pin) || own.contains(&stale.pin);
        let known = |stale: &StaleAnchor| pinned(stale) && stale.change != AnchorChange::Newer;
        if anchors.is_empty() || !anchors.iter().all(known) {
            return Err(invalid());
        }
        Ok(Target {
            kind: NEEDS_UPDATE,
            doc: Some(doc),
            base: Some(version_number(base, invalid)?),
            ..Target::default()
        })
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

/// Whether `to` is `from` with some of its pins replaced, each in its place, its links to them
/// relinked, and nothing else changed but the pins' commit and, when unsaid, their repository.
fn reanchors(from: &DocContent, to: &DocContent) -> bool {
    let (Some(was), Some(now)) = (&from.anchors, &to.anchors) else {
        return false;
    };
    let body = reanchored(&from.body, was.pins.iter().zip(&now.pins));
    let repo = was.repo.is_none() || was.repo == now.repo;
    from.title == to.title && was.pins.len() == now.pins.len() && body == to.body && repo
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

#[cfg(test)]
mod tests {
    use weacon_model::Doc;
    use weacon_protocol::ProposalOutcome;
    use weacon_testing::TempDir;

    use super::super::tests::{edit_p, pinned, revise, workspaces};
    use super::*;
    use crate::db::test_support::{by, code, id};
    use crate::test_support::member_id;
    use crate::test_support::proposals::proposed;

    const GO: &str = "sym:a::go#0123456789abcdef";
    const RUN: &str = "sym:a::run#0123456789abcdef";
    const WENT: &str = "sym:a::went#0123456789abcdef";

    #[tokio::test]
    async fn a_core_edits_a_doc_only_to_reanchor_its_pins_or_its_own_open_reanchors() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let text = doc_text(&pinned("P", "[a](sym:a::go) [b](sym:a::run)\n", &[GO, RUN]));
        one.add_doc(&Doc::new(id("p"), text.unwrap()), &by("m1"))
            .await
            .unwrap();
        let core = |proposal: &str, content| Proposed {
            agent: None,
            ..proposed(proposal, "mc1", edit_p(content))
        };
        let mut elsewhere = pinned("P", "[a](sym:a::went) [b](sym:a::run)\n", &[WENT, RUN]);
        if let Some(anchors) = &mut elsewhere.anchors {
            anchors.repo = Some("other".parse().unwrap());
        }
        let bare = Proposed {
            agent: None,
            ..proposed("bare", "mc1", revise("d1", 1, "Other\n"))
        };
        let refused = [
            pinned("Q", "[a](sym:a::went) [b](sym:a::run)\n", &[WENT, RUN]),
            pinned("P", "[a](sym:a::went), [b](sym:a::run)\n", &[WENT, RUN]),
            pinned("P", "[a](sym:a::went)\n", &[WENT]),
            pinned("P", "[a](sym:a::go) [b](sym:a::went)\n", &[WENT, GO]),
            elsewhere,
        ];
        let later = pinned(
            "P",
            "[a](sym:a::gone) [b](sym:a::run)\n",
            &["sym:a::gone#0123456789abcdef", RUN],
        );

        for content in refused {
            let fault = one.propose(&core("bad", content)).await.unwrap_err();
            assert_eq!(code(&fault), "proposal.invalid");
        }
        let fault = one.propose(&bare).await.unwrap_err();
        assert_eq!(code(&fault), "proposal.invalid");
        let first = pinned("P", "[a](sym:a::went) [b](sym:a::run)\n", &[WENT, RUN]);
        one.propose(&core("first", first)).await.unwrap();
        // From the core's own open re-anchor, which the doc doesn't pin yet.
        one.propose(&core("second", later.clone())).await.unwrap();
        let outcome = one.accept(&id("second"), &member_id("m1"), 1).await;
        let outcome = outcome.unwrap();

        assert!(
            matches!(outcome, ProposalOutcome::Accepted { .. }),
            "{outcome:?}"
        );
        let doc = one.doc(&id("p"), None).await.unwrap();
        assert_eq!(doc.text.anchors(), later.anchors.as_ref());
    }
}
