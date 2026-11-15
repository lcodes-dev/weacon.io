mod accept;
mod read;
mod target;

use sha2::{Digest, Sha256};
use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{
    AgentRef, Author, DocId, MachineId, MemberId, ProposalChange, ProposalId, TaskId, code,
};

use super::changes::Versioned;
use super::{Workspace, author_columns, failed, stored};
use target::Target;

/// The most proposals one member's agents may have open in a workspace.
const MAX_OPEN: u32 = 100;
/// What each proposal counts toward the doc quota besides its change: its row and its feed
/// entries.
const OVERHEAD: usize = 256;
/// The most proposals a list holds, newest first.
const PAGE: u32 = 500;

// A proposal's states and kinds, as stored.
const OPEN: &str = "open";
const ACCEPTED: &str = "accepted";
const REJECTED: &str = "rejected";
const SUPERSEDED: &str = "superseded";
const WITHDRAWN: &str = "withdrawn";
const DOC_EDIT: &str = "doc_edit";
const TASK_EDIT: &str = "task_edit";
const NEW_DOC: &str = "new_doc";
const NEW_TASK: &str = "new_task";

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
        let agent = stored_agent(proposed)?;
        let change = serde_json::to_string(&proposed.change).map_err(|_| invalid())?;
        let hash: [u8; 32] = Sha256::digest(change.as_bytes()).into();
        let mut transaction = self.db.write().await?;
        if self.sent_before(&mut transaction, proposed, &hash).await? {
            return Ok(());
        }
        let target = self.proposal_target(&mut transaction, proposed).await?;
        if target.base.is_some() {
            self.supersede(&mut transaction, proposed, &target).await?;
        }
        self.room_for_proposal(&mut transaction, &proposed.owner)
            .await?;
        // At least what its revision will count, so that accepting it fits in what it frees.
        let bytes = i64::try_from(OVERHEAD + change.len() + agent.len()).map_err(|_| invalid())?;
        self.charge(&mut transaction, bytes).await?;
        sqlx::query(
            "INSERT INTO proposals (workspace_id, proposal_id, kind, doc_id, task_id, new_id, base,
                 change, change_hash, snapshot, bytes, author_member, agent, machine_id, state,
                 at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
        .bind(OPEN)
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
    async fn room_for_proposal(
        &self,
        transaction: &mut SqliteConnection,
        owner: &MemberId,
    ) -> Result<(), Fault> {
        let open: u32 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM proposals
             WHERE workspace_id = ? AND author_member = ? AND state = ?",
        )
        .bind(self.id.as_str())
        .bind(owner.as_str())
        .bind(OPEN)
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

    /// Supersedes the same machine's open edits of the doc or task `target` is to; one of
    /// `doc_id` and `task_id` is bound to null, which matches nothing.
    async fn supersede(
        &self,
        transaction: &mut SqliteConnection,
        proposed: &Proposed,
        target: &Target<'_>,
    ) -> Result<(), Fault> {
        let older: Vec<String> = sqlx::query_scalar(
            "SELECT proposal_id FROM proposals
             WHERE workspace_id = ? AND machine_id = ? AND state = ? AND kind = ?
                 AND (doc_id = ? OR task_id = ?)",
        )
        .bind(self.id.as_str())
        .bind(proposed.machine.as_str())
        .bind(OPEN)
        .bind(target.kind)
        .bind(target.doc.map(DocId::as_str))
        .bind(target.task.map(TaskId::as_str))
        .fetch_all(&mut *transaction)
        .await
        .map_err(failed)?;
        for older in older {
            let older: ProposalId = stored(older)?;
            self.close_proposal(transaction, &older, Closing::Superseded, &proposed.owner)
                .await?;
        }
        Ok(())
    }

    /// What the open proposal `id` counts toward the quota; `proposal.not_found` when there's
    /// no such proposal, and `proposal.closed` when it isn't open.
    async fn open_bytes(
        &self,
        transaction: &mut SqliteConnection,
        id: &ProposalId,
    ) -> Result<i64, Fault> {
        let (state, bytes): (String, i64) = sqlx::query_as(
            "SELECT state, bytes FROM proposals WHERE workspace_id = ? AND proposal_id = ?",
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
        Ok(bytes)
    }

    /// Takes the open proposal `id`'s bytes off the quota.
    async fn release_charge(
        &self,
        transaction: &mut SqliteConnection,
        id: &ProposalId,
    ) -> Result<(), Fault> {
        let bytes = self.open_bytes(transaction, id).await?;
        sqlx::query("UPDATE proposals SET bytes = 0 WHERE workspace_id = ? AND proposal_id = ?")
            .bind(self.id.as_str())
            .bind(id.as_str())
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        self.charge(transaction, -bytes).await
    }

    /// Closes the open proposal `id` as `closing` says: its bytes leave the quota, its change
    /// goes, and the feed hears of it through `member`.
    async fn close_proposal(
        &self,
        transaction: &mut SqliteConnection,
        id: &ProposalId,
        closing: Closing<'_>,
        member: &MemberId,
    ) -> Result<(), Fault> {
        self.release_charge(transaction, id).await?;
        let (state, by, version) = closing.columns();
        sqlx::query(
            "UPDATE proposals SET state = ?, decided_by = ?, version = ?, change = NULL,
                 snapshot = NULL
             WHERE workspace_id = ? AND proposal_id = ?",
        )
        .bind(state)
        .bind(by.map(MemberId::as_str))
        .bind(version)
        .bind(self.id.as_str())
        .bind(id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.record_change(transaction, Versioned::Proposal(id), member)
            .await
    }

    /// Rejects the open proposal `id` on behalf of `by`.
    pub(crate) async fn reject(&self, id: &ProposalId, by: &MemberId) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        let rejected = Closing::Rejected { by };
        self.close_proposal(&mut transaction, id, rejected, by)
            .await?;
        transaction.commit().await.map_err(failed)
    }
}

/// `proposed`'s agent as its versions will store it, checked as any version an agent makes is,
/// so that accepting it can't be refused.
fn stored_agent(proposed: &Proposed) -> Result<String, Fault> {
    let author = Author::Agent {
        owner: proposed.owner.clone(),
        agent: Some(proposed.agent.clone()),
    };
    match author_columns(&author) {
        Some((_, _, Some(agent))) => Ok(agent),
        _ => Err(invalid()),
    }
}

/// How an open proposal closes.
enum Closing<'a> {
    Accepted { by: &'a MemberId, version: i64 },
    Rejected { by: &'a MemberId },
    Superseded,
}

impl Closing<'_> {
    /// The state, the member who decided and the version made, as stored.
    const fn columns(&self) -> (&'static str, Option<&MemberId>, Option<i64>) {
        match *self {
            Self::Accepted { by, version } => (ACCEPTED, Some(by), Some(version)),
            Self::Rejected { by } => (REJECTED, Some(by), None),
            Self::Superseded => (SUPERSEDED, None, None),
        }
    }
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
        AgentKind, AgentOwner, Author, DocContent, ProposalAbout, ProposalRecord, ProposalState,
        TaskEditBody, TaskStatus, WorkspaceChange,
    };
    use weacon_testing::TempDir;

    use super::target::doc_text;
    use super::*;
    use crate::db::test_support::{by, code, id};
    use crate::test_support::proposals::proposed;
    use crate::test_support::{member_id, new_task, two_teams};

    pub(super) fn content(title: &str, body: &str) -> DocContent {
        DocContent {
            title: title.to_owned(),
            body: body.to_owned(),
            anchors: None,
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
        assert_eq!(first.at, 0);
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
            (revise("d1", 1, "One\nTwo\nThree\n"), "proposal.invalid"),
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
        let agent =
            serde_json::to_string(&proposed("p1", "mc1", ProposalChange::Newer).agent).unwrap();

        one.propose(&proposed("p1", "mc1", change)).await.unwrap();
        assert_eq!(
            doc_bytes(&one).await,
            before + i64::try_from(OVERHEAD + json.len() + agent.len()).unwrap()
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

    #[tokio::test]
    async fn an_agent_that_doesnt_answer_to_its_owner_or_isnt_known_is_refused() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let task = || ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };
        let mut someone_elses = proposed("p1", "mc1", task());
        someone_elses.agent.owner = AgentOwner::Member {
            member: member_id("m3"),
        };
        let mut unknown = proposed("p2", "mc1", task());
        unknown.agent.kind = AgentKind::Newer;

        for refused in [someone_elses, unknown] {
            let fault = one.propose(&refused).await.unwrap_err();
            assert_eq!(code(&fault), "proposal.invalid");
        }
        assert!(one.proposals().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_task_edit_must_change_something() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        let todo = ProposalChange::TaskEdit {
            task: id("t1"),
            base: 1,
            edits: vec![TaskEditBody::Status {
                status: TaskStatus::Todo,
            }],
        };

        for change in [retitle("t1", 1, "Write"), todo] {
            let fault = one
                .propose(&proposed("p1", "mc1", change))
                .await
                .unwrap_err();
            assert_eq!(code(&fault), "proposal.invalid");
        }
    }

    #[tokio::test]
    async fn a_member_at_the_most_open_may_still_replace_an_edit() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        one.propose(&proposed("edit", "mc1", revise("d1", 1, "A\n")))
            .await
            .unwrap();
        for n in 1..MAX_OPEN {
            let task = ProposalChange::NewTask {
                title: "Ship".to_owned(),
            };
            one.propose(&proposed(&format!("p{n}"), "mc2", task))
                .await
                .unwrap();
        }

        one.propose(&proposed("newer", "mc1", revise("d1", 1, "B\n")))
            .await
            .unwrap();
        let older = one.proposal(&id("edit")).await.unwrap();
        assert_eq!(older.state, ProposalState::Superseded);
    }

    #[tokio::test]
    async fn a_withdrawn_proposal_reads_as_withdrawn() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        one.propose(&proposed("p1", "mc1", revise("d1", 1, "A\n")))
            .await
            .unwrap();
        sqlx::query("UPDATE proposals SET state = 'withdrawn', change = NULL")
            .execute(&one.db.writer)
            .await
            .unwrap();

        let record = one.proposal(&id("p1")).await.unwrap();
        assert_eq!(
            (record.state, record.change),
            (ProposalState::Withdrawn, None)
        );
    }

    #[tokio::test]
    async fn a_database_fault_reading_what_a_proposal_changes_stays_a_fault() {
        let dir = TempDir::new().unwrap();
        let (one, _) = workspaces(&dir).await;
        for gone in [
            "ALTER TABLE doc_pins RENAME TO doc_pins_gone",
            "ALTER TABLE tasks RENAME TO tasks_gone",
        ] {
            sqlx::query(gone).execute(&one.db.writer).await.unwrap();
        }

        for change in [revise("d1", 1, "A\n"), retitle("t1", 1, "A")] {
            let fault = one
                .propose(&proposed("p1", "mc1", change))
                .await
                .unwrap_err();
            assert_eq!(code(&fault), "db.failed");
        }
    }
}
