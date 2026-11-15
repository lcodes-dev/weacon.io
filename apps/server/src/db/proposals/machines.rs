use weacon_observability::Fault;
use weacon_protocol::{
    AccountId, DocContent, DocId, MachineId, MachineName, MemberId, PinnedDoc, ProposalChange,
    ProposalId,
};

use super::super::{Workspace, failed, parsed, stored, unreadable};
use super::{Closing, DOC_EDIT, OPEN, not_found};

/// How many docs a page of pinned docs holds: each pins at most 256 anchors of at most 512
/// bytes, so a page fits in a frame.
const PAGE_DOCS: usize = 4;

impl Workspace {
    /// The member `account` is here and what its `machine` is called; none unless the account
    /// is a member and the machine is its own.
    pub(crate) async fn machine_member(
        &self,
        account: &AccountId,
        machine: &MachineId,
    ) -> Result<Option<(MemberId, MachineName)>, Fault> {
        sqlx::query_as::<_, (String, String)>(
            "SELECT m.member_id, c.name FROM members AS m
             JOIN machines AS c ON c.account_id = m.account_id
             WHERE m.workspace_id = ? AND m.account_id = ? AND c.machine_id = ?",
        )
        .bind(self.id.as_str())
        .bind(account.as_str())
        .bind(machine.as_str())
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?
        .map(|(member, name)| Ok((stored(member)?, stored(name)?)))
        .transpose()
    }

    /// The docs whose current revision pins code, by id from the first after `after`, a page
    /// of them, and whether more follow.
    pub(crate) async fn pinned_docs(
        &self,
        after: Option<&DocId>,
    ) -> Result<(Vec<PinnedDoc>, bool), Fault> {
        // One snapshot, so no revision lands between the page and its docs.
        let mut reader = self.db.readers.begin().await.map_err(failed)?;
        let mut ids: Vec<String> = sqlx::query_scalar(
            "SELECT docs.doc_id FROM docs JOIN doc_versions USING (workspace_id, doc_id, version)
             WHERE docs.workspace_id = ? AND doc_versions.commit_id IS NOT NULL
                 AND docs.doc_id > ? ORDER BY docs.doc_id LIMIT ?",
        )
        .bind(self.id.as_str())
        .bind(after.map_or("", DocId::as_str))
        .bind(i64::try_from(PAGE_DOCS + 1).map_err(|_| unreadable())?)
        .fetch_all(&mut *reader)
        .await
        .map_err(failed)?;
        let more = ids.len() > PAGE_DOCS;
        ids.truncate(PAGE_DOCS);
        let mut docs = Vec::new();
        for id in ids {
            let doc = self.read_doc(&mut reader, &stored(id)?, None).await?;
            let anchors = doc.text.anchors().cloned().ok_or_else(unreadable)?;
            docs.push(PinnedDoc {
                id: doc.id,
                version: doc.version.get(),
                anchors,
            });
        }
        Ok((docs, more))
    }

    /// What the open re-anchor that `machine` made of its own, from the doc's revision
    /// `version`, would make the doc `id`, if there is one.
    pub(crate) async fn own_edit(
        &self,
        id: &DocId,
        version: u64,
        machine: &MachineId,
    ) -> Result<Option<DocContent>, Fault> {
        let base = i64::try_from(version).map_err(|_| unreadable())?;
        let change: Option<String> = sqlx::query_scalar(
            "SELECT change FROM proposals WHERE workspace_id = ? AND machine_id = ? AND agent = ''
                 AND state = ? AND kind = ? AND doc_id = ? AND base = ?",
        )
        .bind(self.id.as_str())
        .bind(machine.as_str())
        .bind(OPEN)
        .bind(DOC_EDIT)
        .bind(id.as_str())
        .bind(base)
        .fetch_optional(&self.db.readers)
        .await
        .map_err(failed)?;
        let change: Option<ProposalChange> = change.as_deref().map(parsed).transpose()?;
        let Some(ProposalChange::DocEdit { content, .. }) = change else {
            return Ok(None);
        };
        Ok(Some(content))
    }

    /// Withdraws the open proposal `id` that `machine` made, on behalf of its owner `member`;
    /// another machine's is `proposal.not_found`.
    pub(crate) async fn withdraw(
        &self,
        id: &ProposalId,
        machine: &MachineId,
        member: &MemberId,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        let made_by: Option<String> = sqlx::query_scalar(
            "SELECT machine_id FROM proposals WHERE workspace_id = ? AND proposal_id = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        if made_by.as_deref() != Some(machine.as_str()) {
            return Err(not_found());
        }
        self.close_proposal(&mut transaction, id, Closing::Withdrawn, member)
            .await?;
        transaction.commit().await.map_err(failed)
    }
}

/// A "needs an update" `change` with the findings of the `older` ones it supersedes that it
/// lacks, from the same revision, so that a later move doesn't hide what an earlier one found;
/// none for any other change.
pub(super) fn kept_findings(
    change: &ProposalChange,
    older: Vec<ProposalChange>,
) -> Option<ProposalChange> {
    let ProposalChange::NeedsUpdate { doc, base, anchors } = change else {
        return None;
    };
    let mut kept = anchors.clone();
    for older in older {
        let ProposalChange::NeedsUpdate {
            base: older_base,
            anchors: older,
            ..
        } = older
        else {
            continue;
        };
        for stale in older {
            if older_base == *base && !kept.iter().any(|kept| kept.pin == stale.pin) {
                kept.push(stale);
            }
        }
    }
    Some(ProposalChange::NeedsUpdate {
        doc: doc.clone(),
        base: *base,
        anchors: kept,
    })
}

#[cfg(test)]
mod tests {
    use weacon_model::{Doc, DocText, Title};
    use weacon_protocol::{DocAnchors, ProposalChange, ProposalState};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{by, code, id};
    use crate::test_support::proposals::proposed;
    use crate::test_support::two_teams;

    fn text(pinned: bool) -> DocText {
        let title = Title::try_from("Plan".to_owned()).unwrap();
        if !pinned {
            return DocText::new(title, "Plain.\n".to_owned(), None).unwrap();
        }
        let anchors = DocAnchors {
            commit: "89abcdef0123456789abcdef0123456789abcdef".parse().unwrap(),
            pins: vec!["sym:a::go#0123456789abcdef".parse().unwrap()],
        };
        DocText::new(title, "[go](sym:a::go)\n".to_owned(), Some(anchors)).unwrap()
    }

    fn ids(docs: &[PinnedDoc]) -> Vec<&str> {
        docs.iter().map(|doc| doc.id.as_str()).collect()
    }

    #[tokio::test]
    async fn pinned_docs_come_a_page_at_a_time_by_id() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        for doc in ["d1", "d2", "d3", "d4", "d5"] {
            one.add_doc(&Doc::new(id(doc), text(true)), &by("m1"))
                .await
                .unwrap();
        }
        one.add_doc(&Doc::new(id("d0"), text(false)), &by("m1"))
            .await
            .unwrap();
        two.add_doc(&Doc::new(id("d9"), text(true)), &by("m2"))
            .await
            .unwrap();

        let (first, more) = one.pinned_docs(None).await.unwrap();
        assert_eq!((ids(&first), more), (vec!["d1", "d2", "d3", "d4"], true));
        let (rest, more) = one.pinned_docs(Some(&id("d4"))).await.unwrap();
        assert_eq!((ids(&rest), more), (vec!["d5"], false));
        assert_eq!(first[0].version, 1);
    }

    #[tokio::test]
    async fn only_the_machine_that_proposed_withdraws_and_only_its_owners_machine_is_a_member() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        one.propose(&proposed("p1", "mc1", task)).await.unwrap();

        let other = one
            .withdraw(&id("p1"), &id("mc2"), &id("m1"))
            .await
            .unwrap_err();
        assert_eq!(code(&other), "proposal.not_found");
        one.withdraw(&id("p1"), &id("mc1"), &id("m1"))
            .await
            .unwrap();
        let record = one.proposal(&id("p1")).await.unwrap();
        assert_eq!(record.state, ProposalState::Withdrawn);
        let again = one
            .withdraw(&id("p1"), &id("mc1"), &id("m1"))
            .await
            .unwrap_err();
        assert_eq!(code(&again), "proposal.closed");
        let account: AccountId = id("nobody");
        assert_eq!(
            one.machine_member(&account, &id("mc1")).await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn a_full_page_of_pinned_docs_has_no_more_when_none_follow() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        for doc in ["d1", "d2", "d3", "d4"] {
            one.add_doc(&Doc::new(id(doc), text(true)), &by("m1"))
                .await
                .unwrap();
        }

        let (docs, more) = one.pinned_docs(None).await.unwrap();

        assert_eq!((docs.len(), more), (PAGE_DOCS, false));
    }
}
