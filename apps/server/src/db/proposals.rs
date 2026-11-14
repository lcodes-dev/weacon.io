mod accept;
mod read;
mod target;

use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{
    AgentRef, DocId, MachineId, MemberId, ProposalChange, ProposalId, TaskId, code,
};

use super::changes::Versioned;
use super::{Workspace, failed, stored};
use target::Target;

/// The most proposals one member's agents may have open in a workspace.
const MAX_OPEN: u32 = 100;
/// What each proposal counts toward the doc quota besides its change: its row and its feed
/// entries.
const OVERHEAD: usize = 256;
/// The most proposals a list holds, newest first.
const PAGE: u32 = 500;

/// A proposal as a core sent it, with who the server knows sent it.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the machine socket sends proposals in its own change"
    )
)]
pub(crate) struct Proposed {
    pub(crate) id: ProposalId,
    pub(crate) change: ProposalChange,
    /// The member whose agent proposes it, as `agent` names them.
    pub(crate) owner: MemberId,
    pub(crate) agent: AgentRef,
    pub(crate) machine: MachineId,
    /// The id a new doc or task gets once accepted.
    pub(crate) fresh: String,
    pub(crate) at: i64,
}

impl Workspace {
    /// Stores `proposed` as open. The same id sent again by the same machine with the same
    /// change does nothing; with another change or from another machine it is
    /// `proposal.conflict`. An edit supersedes the same machine's open edit of the same doc or
    /// task.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the machine socket sends proposals in its own change"
        )
    )]
    pub(crate) async fn propose(&self, proposed: &Proposed) -> Result<(), Fault> {
        let change = serde_json::to_string(&proposed.change).map_err(|_| invalid())?;
        let hash: [u8; 32] = Sha256::digest(change.as_bytes()).into();
        let mut transaction = self.db.write().await?;
        if self.sent_before(&mut transaction, proposed, &hash).await? {
            return Ok(());
        }
        let target = self.target(&mut transaction, proposed).await?;
        self.room_for(&mut transaction, &proposed.owner).await?;
        if target.base.is_some() {
            self.supersede(&mut transaction, proposed, &target).await?;
        }
        let bytes = i64::try_from(OVERHEAD + change.len()).map_err(|_| invalid())?;
        self.charge(&mut transaction, bytes).await?;
        let agent = serde_json::to_string(&proposed.agent).map_err(|_| invalid())?;
        sqlx::query(
            "INSERT INTO proposals (workspace_id, proposal_id, kind, doc_id, task_id, new_id, base,
                 change, change_hash, snapshot, bytes, author_member, agent, machine_id, state,
                 at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'open', ?)",
        )
        .bind(self.id.as_str())
        .bind(proposed.id.as_str())
        .bind(target.kind)
        .bind(target.doc.map(DocId::as_str))
        .bind(target.task.map(TaskId::as_str))
        .bind(target.new_id)
        .bind(target.base)
        .bind(&change)
        .bind(hash.as_slice())
        .bind(&target.snapshot)
        .bind(bytes)
        .bind(proposed.owner.as_str())
        .bind(agent)
        .bind(proposed.machine.as_str())
        .bind(proposed.at)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        let versioned = Versioned::Proposal(&proposed.id);
        self.record_change(&mut transaction, versioned, &proposed.owner)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// Whether the same machine sent the same proposal before; `proposal.conflict` when its
    /// id is taken otherwise.
    async fn sent_before(
        &self,
        transaction: &mut SqliteConnection,
        proposed: &Proposed,
        hash: &[u8],
    ) -> Result<bool, Fault> {
        let earlier: Option<(String, Vec<u8>)> = sqlx::query_as(
            "SELECT machine_id, change_hash FROM proposals
             WHERE workspace_id = ? AND proposal_id = ?",
        )
        .bind(self.id.as_str())
        .bind(proposed.id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?;
        match earlier {
            None => Ok(false),
            Some((machine, earlier)) if machine == proposed.machine.as_str() && earlier == hash => {
                Ok(true)
            }
            Some(_) => Err(conflict()),
        }
    }

    /// Refuses a proposal from a member whose agents have [`MAX_OPEN`] open already.
    async fn room_for(
        &self,
        transaction: &mut SqliteConnection,
        owner: &MemberId,
    ) -> Result<(), Fault> {
        let open: u32 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proposals
             WHERE workspace_id = ? AND author_member = ? AND state = 'open'",
        )
        .bind(self.id.as_str())
        .bind(owner.as_str())
        .fetch_one(&mut *transaction)
        .await
        .map_err(failed)?;
        if open >= MAX_OPEN {
            return Err(Fault::new(
                code!("proposal.too_many"),
                "The member's agents have as many proposals open as they may",
            ));
        }
        Ok(())
    }

    /// Supersedes the same machine's open edits of the doc or task `target` is to.
    async fn supersede(
        &self,
        transaction: &mut SqliteConnection,
        proposed: &Proposed,
        target: &Target<'_>,
    ) -> Result<(), Fault> {
        let older: Vec<String> = sqlx::query_scalar(
            "SELECT proposal_id FROM proposals
             WHERE workspace_id = ? AND machine_id = ? AND state = 'open' AND kind = ?
                 AND (doc_id = ? OR task_id = ?)",
        )
        .bind(self.id.as_str())
        .bind(proposed.machine.as_str())
        .bind(target.kind)
        .bind(target.doc.map(DocId::as_str))
        .bind(target.task.map(TaskId::as_str))
        .fetch_all(&mut *transaction)
        .await
        .map_err(failed)?;
        for older in older {
            let older: ProposalId = stored(older)?;
            let closing = Closing {
                state: "superseded",
                by: None,
                version: None,
            };
            self.close(transaction, &older, closing, &proposed.owner)
                .await?;
        }
        Ok(())
    }

    /// Closes the open proposal `id` as `closing` says: its change goes, its bytes leave the
    /// quota, and the feed hears of it through `member`.
    async fn close(
        &self,
        transaction: &mut SqliteConnection,
        id: &ProposalId,
        closing: Closing<'_>,
        member: &MemberId,
    ) -> Result<(), Fault> {
        let bytes: i64 = sqlx::query_scalar(
            "SELECT bytes FROM proposals
             WHERE workspace_id = ? AND proposal_id = ? AND state = 'open'",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?
        .ok_or_else(closed)?;
        sqlx::query(
            "UPDATE proposals SET state = ?, decided_by = ?, version = ?, change = NULL,
                 snapshot = NULL, bytes = 0
             WHERE workspace_id = ? AND proposal_id = ?",
        )
        .bind(closing.state)
        .bind(closing.by.map(MemberId::as_str))
        .bind(closing.version)
        .bind(self.id.as_str())
        .bind(id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.charge(transaction, -bytes).await?;
        self.record_change(transaction, Versioned::Proposal(id), member)
            .await
    }

    /// Rejects the open proposal `id` on behalf of `by`.
    pub(crate) async fn reject(&self, id: &ProposalId, by: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        self.state_of(&mut transaction, id).await?;
        let closing = Closing {
            state: "rejected",
            by: Some(by),
            version: None,
        };
        self.close(&mut transaction, id, closing, by).await?;
        transaction.commit().await.map_err(failed)
    }

    /// The proposal `id`'s state; `proposal.not_found` when there's none, and
    /// `proposal.closed` when it isn't open.
    async fn state_of(
        &self,
        transaction: &mut SqliteConnection,
        id: &ProposalId,
    ) -> Result<(), Fault> {
        let state: String = sqlx::query_scalar(
            "SELECT state FROM proposals WHERE workspace_id = ? AND proposal_id = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?
        .ok_or_else(not_found)?;
        if state == "open" {
            Ok(())
        } else {
            Err(closed())
        }
    }
}

/// How an open proposal closes.
struct Closing<'a> {
    state: &'static str,
    by: Option<&'a MemberId>,
    version: Option<i64>,
}

fn invalid() -> Fault {
    Fault::new(code!("proposal.invalid"), "The proposal isn't valid")
}

fn conflict() -> Fault {
    Fault::new(code!("proposal.conflict"), "Another proposal has that id")
}

pub(crate) fn not_found() -> Fault {
    Fault::new(code!("proposal.not_found"), "There is no such proposal")
}

fn closed() -> Fault {
    Fault::new(code!("proposal.closed"), "The proposal isn't open")
}

#[cfg(test)]
mod tests {
    use weacon_model::Doc;
    use weacon_protocol::{
        AgentKind, AgentOwner, Author, DocContent, MachineName, ProposalAbout, ProposalRecord,
        ProposalState, TaskEditBody, TaskStatus, WorkspaceChange,
    };
    use weacon_testing::TempDir;

    use super::target::doc_text;
    use super::*;
    use crate::db::test_support::{NOW, by, code, id};
    use crate::test_support::{member_id, new_task, two_teams};

    pub(super) fn content(title: &str, body: &str) -> DocContent {
        DocContent {
            title: title.to_owned(),
            body: body.to_owned(),
            anchors: None,
        }
    }

    /// `change`, proposed as `proposal` by m1's agent on the machine `machine`.
    pub(super) fn proposed(proposal: &str, machine: &str, change: ProposalChange) -> Proposed {
        Proposed {
            id: id(proposal),
            change,
            owner: member_id("m1"),
            agent: AgentRef {
                kind: AgentKind::Claude,
                owner: AgentOwner::Member {
                    member: member_id("m1"),
                },
                machine: Some(MachineName::try_from(format!("{machine} laptop")).unwrap()),
            },
            machine: id(machine),
            fresh: format!("new-{proposal}"),
            at: NOW,
        }
    }

    /// w1 and w2, with doc d1 and task t1 in w1.
    pub(super) async fn workspaces(dir: &TempDir) -> (Workspace, Workspace) {
        let (one, two) = two_teams(dir).await;
        let text = doc_text(&content("Plan", "One\nTwo\nThree\n")).unwrap();
        one.add_doc(&Doc::new(id("d1"), text), &by("m1"))
            .await
            .unwrap();
        one.add_task(&new_task("t1", "Write", "a"), &by("m1"))
            .await
            .unwrap();
        (one, two)
    }

    pub(super) fn retitle(task: &str, base: u64, title: &str) -> ProposalChange {
        ProposalChange::TaskEdit {
            task: id(task),
            base,
            edits: vec![TaskEditBody::Title {
                title: title.to_owned(),
            }],
        }
    }

    pub(super) fn revise(doc: &str, base: u64, body: &str) -> ProposalChange {
        ProposalChange::DocEdit {
            doc: id(doc),
            base,
            content: content("Plan", body),
        }
    }

    fn states(records: &[ProposalRecord]) -> Vec<(&str, &ProposalState)> {
        records
            .iter()
            .map(|record| (record.id.as_str(), &record.state))
            .collect()
    }

    #[tokio::test]
    async fn a_proposal_is_stored_open_with_its_author_and_heard_of_on_the_feed() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let before = one.changes_since(0, 100).await.unwrap().len();
        let new_task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };

        one.propose(&proposed("p1", "mc1", revise("d1", 1, "One\n")))
            .await
            .unwrap();
        one.propose(&proposed("p2", "mc1", new_task.clone()))
            .await
            .unwrap();

        let list = one.proposals().await.unwrap();
        assert_eq!(
            states(&list),
            [("p2", &ProposalState::Open), ("p1", &ProposalState::Open)]
        );
        let first = one.proposal(&id("p1")).await.unwrap();
        assert_eq!(first.about, ProposalAbout::Doc { doc: id("d1") });
        assert_eq!(first.change, Some(revise("d1", 1, "One\n")));
        assert_eq!(first.at, NOW);
        let Author::Agent {
            owner,
            agent: Some(agent),
        } = &first.author
        else {
            panic!("{:?}", first.author);
        };
        assert_eq!(
            (owner, agent),
            (&member_id("m1"), &proposed("p1", "mc1", new_task).agent)
        );
        assert_eq!(list[0].about, ProposalAbout::NewTask { task: id("new-p2") });
        let changes = one.changes_since(0, 100).await.unwrap();
        let heard: Vec<_> = changes
            .into_iter()
            .skip(before)
            .map(|update| update.change)
            .collect();
        assert_eq!(
            heard,
            [
                WorkspaceChange::ProposalChanged { proposal: id("p1") },
                WorkspaceChange::ProposalChanged { proposal: id("p2") },
            ]
        );
        let missing = one.proposal(&id("p9")).await.unwrap_err();
        assert_eq!(code(&missing), "proposal.not_found");
    }

    #[tokio::test]
    async fn the_same_proposal_sent_again_is_one_and_its_id_is_taken_otherwise() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let first = proposed("p1", "mc1", revise("d1", 1, "One\n"));
        one.propose(&first).await.unwrap();

        one.propose(&first).await.unwrap();
        assert_eq!(one.proposals().await.unwrap().len(), 1);
        for other in [
            proposed("p1", "mc1", revise("d1", 1, "Two\n")),
            proposed("p1", "mc2", revise("d1", 1, "One\n")),
        ] {
            assert_eq!(
                code(&one.propose(&other).await.unwrap_err()),
                "proposal.conflict"
            );
        }
        one.reject(&id("p1"), &member_id("m3")).await.unwrap();
        one.propose(&first).await.unwrap();
        let record = one.proposal(&id("p1")).await.unwrap();
        assert_eq!(
            record.state,
            ProposalState::Rejected {
                by: member_id("m3")
            }
        );
    }

    #[tokio::test]
    async fn an_edit_supersedes_only_the_same_machines_open_edit_of_the_same_thing() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        one.add_task(&new_task("t2", "Test", "b"), &by("m1"))
            .await
            .unwrap();
        let new_doc = ProposalChange::NewDoc {
            content: content("Notes", "A\n"),
        };
        for (proposal, machine, change) in [
            ("p1", "mc1", revise("d1", 1, "One\n")),
            ("p2", "mc2", revise("d1", 1, "Two\n")),
            ("p3", "mc1", retitle("t1", 1, "Write it")),
            ("p4", "mc1", retitle("t2", 1, "Test it")),
            ("p5", "mc1", new_doc.clone()),
            ("p6", "mc1", new_doc),
            ("p7", "mc1", revise("d1", 1, "Three\n")),
            ("p8", "mc1", retitle("t1", 1, "Write all")),
        ] {
            one.propose(&proposed(proposal, machine, change))
                .await
                .unwrap();
        }

        let list = one.proposals().await.unwrap();
        let open = ProposalState::Open;
        let superseded = ProposalState::Superseded;
        assert_eq!(
            states(&list),
            [
                ("p8", &open),
                ("p7", &open),
                ("p6", &open),
                ("p5", &open),
                ("p4", &open),
                ("p3", &superseded),
                ("p2", &open),
                ("p1", &superseded),
            ]
        );
        assert!(
            list.iter()
                .filter(|record| record.state == superseded)
                .all(|record| record.change.is_none())
        );
    }

    #[tokio::test]
    async fn a_proposal_must_fit_what_the_workspace_holds() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let edits = |edits: Vec<TaskEditBody>| ProposalChange::TaskEdit {
            task: id("t1"),
            base: 1,
            edits,
        };
        let unpinned = ProposalChange::NewDoc {
            content: content("Plan", "See [a](sym:a)."),
        };
        let refusals = [
            (revise("d9", 1, "A\n"), "proposal.invalid"),
            (revise("d1", 0, "A\n"), "proposal.invalid"),
            (revise("d1", 2, "A\n"), "proposal.invalid"),
            (retitle("t9", 1, "A"), "proposal.invalid"),
            (retitle("t1", 2, "A"), "proposal.stale"),
            (retitle("t1", 1, ""), "proposal.invalid"),
            (edits(vec![]), "proposal.invalid"),
            (
                edits(vec![TaskEditBody::Move { before: None }]),
                "proposal.invalid",
            ),
            (edits(vec![TaskEditBody::Newer]), "proposal.invalid"),
            (
                edits(vec![TaskEditBody::Status {
                    status: TaskStatus::Newer,
                }]),
                "proposal.invalid",
            ),
            (unpinned, "proposal.invalid"),
            (
                ProposalChange::NewTask {
                    title: String::new(),
                },
                "proposal.invalid",
            ),
            (ProposalChange::Newer, "proposal.invalid"),
        ];
        for (change, refused) in refusals {
            let fault = one
                .propose(&proposed("p1", "mc1", change.clone()))
                .await
                .unwrap_err();
            assert_eq!(code(&fault), refused, "{change:?}");
        }
        assert!(one.proposals().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_new_doc_or_task_needs_an_id_it_can_have() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let task = ProposalChange::NewTask {
            title: "A".to_owned(),
        };
        let doc = ProposalChange::NewDoc {
            content: content("A", ""),
        };
        for change in [task, doc] {
            let mut odd = proposed("p1", "mc1", change);
            odd.fresh = "a b".to_owned();
            let fault = one.propose(&odd).await.unwrap_err();
            assert_eq!(code(&fault), "proposal.invalid");
        }
    }

    #[tokio::test]
    async fn one_members_agents_hold_only_so_many_open_proposals() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let task = || ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        for n in 0..MAX_OPEN {
            one.propose(&proposed(&format!("p{n}"), "mc1", task()))
                .await
                .unwrap();
        }

        let past = one
            .propose(&proposed("over", "mc2", task()))
            .await
            .unwrap_err();
        assert_eq!(code(&past), "proposal.too_many");
        let mut bo = proposed("bo", "mc3", task());
        bo.owner = member_id("m3");
        bo.agent.owner = AgentOwner::Member {
            member: member_id("m3"),
        };
        one.propose(&bo).await.unwrap();
        one.reject(&id("p0"), &member_id("m3")).await.unwrap();
        one.propose(&proposed("again", "mc1", task()))
            .await
            .unwrap();
    }

    async fn doc_bytes(workspace: &Workspace) -> i64 {
        sqlx::query_scalar("SELECT doc_bytes FROM workspaces WHERE workspace_id = ?")
            .bind(workspace.id.as_str())
            .fetch_one(&workspace.db.readers)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn an_open_proposal_counts_toward_the_doc_quota_until_it_closes() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let before = doc_bytes(&one).await;
        let change = revise("d1", 1, "One\n");
        let json = serde_json::to_string(&change).unwrap();

        one.propose(&proposed("p1", "mc1", change)).await.unwrap();
        assert_eq!(
            doc_bytes(&one).await,
            before + i64::try_from(OVERHEAD + json.len()).unwrap()
        );
        one.propose(&proposed("p2", "mc1", revise("d1", 1, "Two\n")))
            .await
            .unwrap();
        one.reject(&id("p2"), &member_id("m1")).await.unwrap();
        assert_eq!(doc_bytes(&one).await, before);
        one.fill_doc_quota().await;
        let full = one
            .propose(&proposed("p3", "mc1", revise("d1", 1, "A\n")))
            .await
            .unwrap_err();
        assert_eq!(code(&full), "doc.quota_full");
    }

    #[tokio::test]
    async fn only_an_open_proposal_is_rejected() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        one.propose(&proposed("p1", "mc1", revise("d1", 1, "One\n")))
            .await
            .unwrap();

        one.reject(&id("p1"), &member_id("m3")).await.unwrap();
        let record = one.proposal(&id("p1")).await.unwrap();
        assert_eq!(
            record.state,
            ProposalState::Rejected {
                by: member_id("m3")
            }
        );
        assert_eq!(record.change, None);
        let again = one.reject(&id("p1"), &member_id("m1")).await.unwrap_err();
        assert_eq!(code(&again), "proposal.closed");
        let missing = one.reject(&id("p9"), &member_id("m1")).await.unwrap_err();
        assert_eq!(code(&missing), "proposal.not_found");
    }

    #[tokio::test]
    async fn workspaces_keep_their_proposals_apart() {
        let dir = TempDir::new().unwrap();
        let (one, two) = workspaces(&dir).await;
        let task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        one.propose(&proposed("p1", "mc1", task.clone()))
            .await
            .unwrap();

        assert!(two.proposals().await.unwrap().is_empty());
        let unseen = two.proposal(&id("p1")).await.unwrap_err();
        assert_eq!(code(&unseen), "proposal.not_found");
        let refused = two.reject(&id("p1"), &member_id("m2")).await.unwrap_err();
        assert_eq!(code(&refused), "proposal.not_found");
        // w1's doc and task aren't w2's to propose to.
        let mut theirs = proposed("p1", "mc1", revise("d1", 1, "One\n"));
        theirs.owner = member_id("m2");
        theirs.agent.owner = AgentOwner::Member {
            member: member_id("m2"),
        };
        assert_eq!(
            code(&two.propose(&theirs).await.unwrap_err()),
            "proposal.invalid"
        );
        theirs.change = task;
        two.propose(&theirs).await.unwrap();
        assert_eq!(one.proposals().await.unwrap().len(), 1);
    }
}
