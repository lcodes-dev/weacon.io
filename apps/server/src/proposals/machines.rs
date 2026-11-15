//! What a paired machine's core proposes and asks, for its owner, in a workspace they are a
//! member of.

use weacon_observability::{Fault, Field};
use weacon_protocol::{
    AgentOwner, AgentRef, Code, MachineName, MemberId, Outcome, ProposalAction, ProposalAnswer,
    ProposalAnswerKind, ProposalCommand, ProposalQuery, ProposalQueryKind, Receipt, WorkspaceId,
    code,
};

use crate::app::new_id;
use crate::auth::Machine;
use crate::db::{Proposed, Workspace};
use crate::docs::record;
use crate::shared::Shared;

use super::invalid;

/// The workspace and the member the machine's owner is there, refused the same way whether
/// the workspace is missing or they aren't in it, so a core learns of no other workspace.
async fn member(
    shared: &Shared,
    machine: &Machine,
    workspace: &WorkspaceId,
) -> Result<(Workspace, MemberId, MachineName), Fault> {
    let not_member = || {
        Fault::new(
            code!("workspace.not_found"),
            "The machine's owner isn't a member of the workspace",
        )
    };
    let workspace = shared
        .db
        .find_workspace(workspace)
        .await?
        .ok_or_else(not_member)?;
    let (member, name) = workspace
        .machine_member(&machine.owner, &machine.id)
        .await?
        .ok_or_else(not_member)?;
    Ok((workspace, member, name))
}

/// Stores or withdraws what `command` proposes, and tells open feeds.
pub(crate) async fn command(
    shared: &Shared,
    machine: &Machine,
    command: ProposalCommand,
) -> Receipt {
    let request = command.request.clone();
    let outcome = match proposed(shared, machine, command).await {
        Ok(()) => Outcome::Completed,
        Err(fault) => Outcome::Failed {
            code: refused(shared, &fault),
        },
    };
    Receipt { request, outcome }
}

async fn proposed(
    shared: &Shared,
    machine: &Machine,
    command: ProposalCommand,
) -> Result<(), Fault> {
    let (workspace, member, name) = member(shared, machine, &command.workspace).await?;
    let made = |id, change, agent| Proposed {
        id,
        change,
        owner: member.clone(),
        agent,
        machine: machine.id.clone(),
        fresh: new_id(),
        at: shared.now(),
    };
    match command.action {
        ProposalAction::Propose {
            proposal,
            agent,
            change,
        } => {
            let agent = AgentRef {
                kind: agent,
                owner: AgentOwner::Member {
                    member: member.clone(),
                },
                machine: Some(name),
            };
            workspace
                .propose(&made(proposal, change, Some(agent)))
                .await?;
        }
        ProposalAction::Batch { proposals } => {
            if proposals.is_empty() {
                return Err(invalid());
            }
            let batch: Vec<Proposed> = proposals
                .into_iter()
                .map(|batched| made(batched.proposal, batched.change, None))
                .collect();
            workspace.propose_all(&batch).await?;
        }
        ProposalAction::Withdraw { proposal } => {
            workspace.withdraw(&proposal, &machine.id, &member).await?;
        }
        ProposalAction::Newer => return Err(invalid()),
    }
    shared.wake_feeds(workspace.id());
    Ok(())
}

/// The answer to `query`, under its request.
pub(crate) async fn query(
    shared: &Shared,
    machine: &Machine,
    query: ProposalQuery,
) -> ProposalAnswer {
    let answer = match answer(shared, machine, &query).await {
        Ok(answer) => answer,
        Err(fault) => ProposalAnswerKind::Failed {
            code: refused(shared, &fault),
        },
    };
    ProposalAnswer {
        request: query.request,
        answer,
    }
}

async fn answer(
    shared: &Shared,
    machine: &Machine,
    query: &ProposalQuery,
) -> Result<ProposalAnswerKind, Fault> {
    let (workspace, _, _) = member(shared, machine, &query.workspace).await?;
    match &query.query {
        ProposalQueryKind::Pinned { after } => {
            let (docs, more) = workspace.pinned_docs(after.as_ref()).await?;
            Ok(ProposalAnswerKind::Pinned { docs, more })
        }
        ProposalQueryKind::Doc { doc } => {
            let doc = workspace.doc(doc, None).await?;
            Ok(ProposalAnswerKind::Doc { doc: record(doc) })
        }
        ProposalQueryKind::Newer => Err(invalid()),
    }
}

/// The code a refusal answers with; a failure that isn't the core's to fix is logged too.
fn refused(shared: &Shared, fault: &Fault) -> Code {
    let theirs = ["proposal.", "doc.", "task.", "workspace."];
    if !theirs
        .iter()
        .any(|prefix| fault.code().as_str().starts_with(prefix))
    {
        shared
            .log
            .error("proposal failed", &[("err", Field::error(fault))]);
    }
    fault.code().clone()
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use weacon_model::{Attribution, Doc, DocText, Title};
    use weacon_protocol::{
        AgentKind, AnchorChange, Author, Batched, DocAnchors, DocContent, DocId, Envelope,
        MAX_PROPOSAL, Message, ProposalChange, ProposalState, StaleAnchor,
    };

    use super::*;
    use crate::test_support::socket::{Socket, next, next_envelope, open, say, send};
    use crate::test_support::{
        Fixture, email, fresh_account, member_id, messages, pair_machine, workspace_id,
    };

    const PIN: &str = "sym:a::go#0123456789abcdef";
    const BODY: &str = "See [go](sym:a::go).\n";

    fn pinned_text() -> DocText {
        let anchors = DocAnchors {
            commit: "89abcdef0123456789abcdef0123456789abcdef".parse().unwrap(),
            pins: vec![PIN.parse().unwrap()],
        };
        DocText::new(
            Title::try_from("Plan".to_owned()).unwrap(),
            BODY.to_owned(),
            Some(anchors),
        )
        .unwrap()
    }

    /// d1 pinning [`PIN`] in w1, and w2, which ada isn't in, with its own d2.
    async fn docs(fixture: &Fixture) {
        let db = &fixture.shared.db;
        let one = db.workspace(&workspace_id("w1")).await.unwrap();
        let by = Attribution::by(member_id("m1"), 0);
        one.add_doc(&Doc::new(id("d1"), pinned_text()), &by)
            .await
            .unwrap();
        let two = db
            .create_workspace(&workspace_id("w2"), "Other")
            .await
            .unwrap();
        let bo = email("bo@example.com");
        two.add_member(&member_id("m2"), &fresh_account(), &bo)
            .await
            .unwrap();
        let by = Attribution::by(member_id("m2"), 0);
        two.add_doc(&Doc::new(id("d2"), pinned_text()), &by)
            .await
            .unwrap();
    }

    fn id<T: TryFrom<String>>(text: &str) -> T {
        T::try_from(text.to_owned()).ok().unwrap()
    }

    /// ada's machine, connected and welcomed.
    async fn connected(fixture: &Fixture, address: SocketAddr) -> Socket {
        let paired = pair_machine(fixture, address).await;
        let mut core = open(address, "/api/machines/connect", &paired.token)
            .await
            .unwrap();
        say(&mut core, &["machine.connect", "workspace.proposals"]).await;
        assert!(matches!(next(&mut core).await, Message::Welcome(_)));
        core
    }

    /// What the server answers `message`, under its envelope's id.
    async fn asked(core: &mut Socket, message: Message) -> Message {
        send(core, Envelope { id: 7, message }).await;
        let answer = next_envelope(core).await;
        assert_eq!(answer.id, 7);
        answer.message
    }

    async fn commanded(core: &mut Socket, workspace: &str, action: ProposalAction) -> Outcome {
        let command = Message::ProposalCommand(ProposalCommand {
            request: id("r1"),
            workspace: workspace_id(workspace),
            action,
        });
        let Message::Receipt(receipt) = asked(core, command).await else {
            panic!("no receipt");
        };
        assert_eq!(receipt.request, id("r1"));
        receipt.outcome
    }

    async fn queried(
        core: &mut Socket,
        workspace: &str,
        query: ProposalQueryKind,
    ) -> ProposalAnswerKind {
        let query = Message::ProposalQuery(ProposalQuery {
            request: id("r2"),
            workspace: workspace_id(workspace),
            query,
        });
        let Message::ProposalAnswer(answer) = asked(core, query).await else {
            panic!("no answer");
        };
        answer.answer
    }

    fn propose(proposal: &str, change: ProposalChange) -> ProposalAction {
        ProposalAction::Propose {
            proposal: id(proposal),
            agent: AgentKind::Claude,
            change,
        }
    }

    fn failed(code: &str) -> Outcome {
        Outcome::Failed {
            code: serde_json::from_value(serde_json::json!(code)).unwrap(),
        }
    }

    /// An edit of `doc` at its first revision that drops its link.
    fn edit(doc: &str) -> ProposalChange {
        ProposalChange::DocEdit {
            doc: id(doc),
            base: 1,
            content: DocContent {
                title: "Plan".to_owned(),
                body: "Gone.\n".to_owned(),
                anchors: None,
            },
        }
    }

    fn needs_update(doc: &str) -> ProposalChange {
        ProposalChange::NeedsUpdate {
            doc: id(doc),
            base: 1,
            anchors: vec![StaleAnchor {
                pin: PIN.parse().unwrap(),
                change: AnchorChange::Gone {
                    was: "fn a::go  src/lib.rs:1".to_owned(),
                },
            }],
        }
    }

    #[tokio::test]
    async fn an_agents_proposal_names_its_owner_and_machine_and_waits_for_a_person() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let mut core = connected(&fixture, server.address).await;
        let mut changes = fixture.shared.changes.subscribe();
        let task = ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };

        let outcome = commanded(&mut core, "w1", propose("p1", task)).await;

        assert_eq!(outcome, Outcome::Completed);
        assert_eq!(changes.try_recv().unwrap(), workspace_id("w1"));
        let one = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let record = one.proposal(&id("p1")).await.unwrap();
        let agent = AgentRef {
            kind: AgentKind::Claude,
            owner: AgentOwner::Member {
                member: member_id("m1"),
            },
            machine: Some(id("Studio")),
        };
        assert_eq!(
            (record.author, record.state),
            (
                Author::Agent {
                    owner: member_id("m1"),
                    agent: Some(agent)
                },
                ProposalState::Open
            )
        );
        assert!(one.tasks().await.unwrap().is_empty());
        one.accept(&id("p1"), &member_id("m1"), 0).await.unwrap();
        assert_eq!(one.tasks().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_core_reads_the_pinned_docs_a_page_at_a_time_and_one_doc() {
        let fixture = Fixture::new().await;
        docs(&fixture).await;
        let server = fixture.run().await;
        let mut core = connected(&fixture, server.address).await;

        let pinned = queried(&mut core, "w1", ProposalQueryKind::Pinned { after: None }).await;
        let after = ProposalQueryKind::Pinned {
            after: Some(id("d1")),
        };
        let past = queried(&mut core, "w1", after).await;
        let doc = queried(&mut core, "w1", ProposalQueryKind::Doc { doc: id("d1") }).await;

        let ProposalAnswerKind::Pinned { docs, more: false } = pinned else {
            panic!("{pinned:?}");
        };
        let ids: Vec<&DocId> = docs.iter().map(|doc| &doc.id).collect();
        assert_eq!(
            (ids, docs[0].anchors.pins[0].as_str()),
            (vec![&id("d1")], PIN)
        );
        assert_eq!(
            past,
            ProposalAnswerKind::Pinned {
                docs: vec![],
                more: false
            }
        );
        assert!(matches!(doc, ProposalAnswerKind::Doc { doc } if doc.content.body == BODY));
    }

    #[tokio::test]
    async fn a_core_proposes_a_batch_for_one_doc_and_withdraws_from_it() {
        let fixture = Fixture::new().await;
        docs(&fixture).await;
        let server = fixture.run().await;
        let mut core = connected(&fixture, server.address).await;
        let proposals = [("b1", needs_update("d1")), ("b2", edit("d1"))]
            .map(|(proposal, change)| Batched {
                proposal: id(proposal),
                change,
            })
            .to_vec();
        let outcome = commanded(&mut core, "w1", ProposalAction::Batch { proposals }).await;
        let withdrawn = ProposalAction::Withdraw { proposal: id("b2") };
        let withdrew = commanded(&mut core, "w1", withdrawn).await;

        assert_eq!(
            (outcome, withdrew),
            (Outcome::Completed, Outcome::Completed)
        );
        let one = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let by_core = Author::Agent {
            owner: member_id("m1"),
            agent: None,
        };
        let records = one.proposals().await.unwrap();
        let states: Vec<_> = records
            .iter()
            .map(|record| (record.id.as_str(), &record.state))
            .collect();
        assert_eq!(
            states,
            [
                ("b2", &ProposalState::Withdrawn),
                ("b1", &ProposalState::Open)
            ]
        );
        assert!(records.iter().all(|record| record.author == by_core));
        let seen = one.accept(&id("b1"), &member_id("m1"), 0).await.unwrap();
        assert_eq!(
            seen,
            weacon_protocol::ProposalOutcome::Accepted { version: 1 }
        );
        assert_eq!(one.doc(&id("d1"), None).await.unwrap().text.body(), BODY);
    }

    #[tokio::test]
    async fn a_core_is_refused_outside_its_owners_workspaces_and_for_another_workspaces_doc() {
        let fixture = Fixture::new().await;
        docs(&fixture).await;
        let server = fixture.run().await;
        let mut core = connected(&fixture, server.address).await;
        let task = || ProposalChange::NewTask {
            title: "Ship".to_owned(),
        };

        for workspace in ["w2", "w9"] {
            let outcome = commanded(&mut core, workspace, propose("p1", task())).await;
            assert_eq!(outcome, failed("workspace.not_found"), "{workspace}");
            let pinned = ProposalQueryKind::Pinned { after: None };
            let answer = queried(&mut core, workspace, pinned).await;
            assert_eq!(
                answer,
                ProposalAnswerKind::Failed {
                    code: code!("workspace.not_found")
                }
            );
        }
        for change in [needs_update("d2"), edit("d2")] {
            let outcome = commanded(&mut core, "w1", propose("p2", change)).await;
            assert_eq!(outcome, failed("proposal.invalid"));
        }
        let two = fixture
            .shared
            .db
            .workspace(&workspace_id("w2"))
            .await
            .unwrap();
        assert!(two.proposals().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_proposal_too_large_or_an_empty_batch_is_refused() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;
        let mut core = connected(&fixture, server.address).await;
        let large = ProposalChange::NewTask {
            title: "x".repeat(MAX_PROPOSAL),
        };

        let too_large = commanded(&mut core, "w1", propose("p1", large)).await;
        let empty = commanded(&mut core, "w1", ProposalAction::Batch { proposals: vec![] }).await;
        let elsewhere = commanded(
            &mut core,
            "w1",
            ProposalAction::Withdraw { proposal: id("p9") },
        )
        .await;

        assert_eq!(too_large, failed("proposal.too_large"));
        assert_eq!(empty, failed("proposal.invalid"));
        assert_eq!(elsewhere, failed("proposal.not_found"));
        assert!(!messages(&fixture.records).contains(&"proposal failed"));
    }

    #[tokio::test]
    async fn only_a_failure_that_is_not_the_cores_to_fix_is_logged() {
        let fixture = Fixture::new().await;
        let codes: Vec<Code> = [
            code!("db.failed"),
            code!("proposal.invalid"),
            code!("doc.stale"),
        ]
        .into_iter()
        .map(|code| refused(&fixture.shared, &Fault::new(code, "Refused")))
        .collect();

        assert_eq!(
            codes,
            [
                code!("db.failed"),
                code!("proposal.invalid"),
                code!("doc.stale")
            ]
        );
        assert_eq!(messages(&fixture.records), ["proposal failed"]);
    }
}
