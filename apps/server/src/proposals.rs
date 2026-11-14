//! Agents' proposals to docs and tasks, which members accept or reject.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use weacon_observability::Fault;
use weacon_protocol::{
    Code, ProposalDecision, ProposalId, ProposalList, ProposalOutcome, ProposalRecord,
    ProposalVerdict, WorkspaceId, code,
};

use crate::auth::Member;
use crate::http::{self, Body, Refusals, Unreadable, parsed};
use crate::shared::Shared;

/// What the server offers to clients that read and decide on proposals.
pub(crate) const PROPOSALS: Code = code!("workspace.proposals");

pub(crate) fn routes() -> Router<Shared> {
    Router::new()
        .route("/api/workspaces/{workspace}/proposals", get(list))
        .route(
            "/api/workspaces/{workspace}/proposals/{proposal}",
            get(read).post(decide),
        )
}

/// `GET /api/workspaces/{workspace}/proposals`: the workspace's newest proposals, newest
/// first.
async fn list(member: Member) -> Result<Json<ProposalList>, Refused> {
    let proposals = member.workspace.proposals().await?;
    Ok(Json(ProposalList { proposals }))
}

/// `GET /api/workspaces/{workspace}/proposals/{proposal}`: one proposal.
async fn read(
    member: Member,
    path: Result<Path<(WorkspaceId, ProposalId)>, PathRejection>,
) -> Result<Json<ProposalRecord>, Refused> {
    let Path((_, id)) = path.map_err(|_| invalid())?;
    Ok(Json(member.workspace.proposal(&id).await?))
}

/// `POST /api/workspaces/{workspace}/proposals/{proposal}`: a member accepts or rejects an
/// open proposal. Every member edits every doc and task, so any member may decide.
async fn decide(
    State(shared): State<Shared>,
    member: Member,
    path: Result<Path<(WorkspaceId, ProposalId)>, PathRejection>,
    body: Result<Body<ProposalDecision>, Unreadable>,
) -> Result<Json<ProposalOutcome>, Refused> {
    let Path((_, id)) = path.map_err(|_| invalid())?;
    let decision = parsed(body, invalid, invalid)?;
    let workspace = &member.workspace;
    let outcome = match decision.verdict {
        ProposalVerdict::Accept => workspace.accept(&id, &member.id, shared.now()).await?,
        ProposalVerdict::Reject => {
            workspace.reject(&id, &member.id).await?;
            ProposalOutcome::Rejected
        }
        ProposalVerdict::Newer => return Err(invalid().into()),
    };
    if matches!(
        outcome,
        ProposalOutcome::Accepted { .. } | ProposalOutcome::Rejected
    ) {
        shared.wake_feeds(workspace.id());
    }
    Ok(Json(outcome))
}

fn invalid() -> Fault {
    Fault::new(
        code!("proposal.invalid"),
        "The proposal request isn't valid",
    )
}

struct Proposals;

impl Refusals for Proposals {
    const REFUSALS: &'static [(Code, StatusCode, bool)] = &[
        (code!("proposal.invalid"), StatusCode::BAD_REQUEST, false),
        (code!("proposal.not_found"), StatusCode::NOT_FOUND, false),
        (code!("proposal.closed"), StatusCode::CONFLICT, false),
        // The doc was revised while the edit merged; accepting again merges with that.
        (code!("doc.stale"), StatusCode::CONFLICT, true),
        (code!("doc.too_large"), StatusCode::PAYLOAD_TOO_LARGE, false),
        (
            code!("doc.quota_full"),
            StatusCode::PAYLOAD_TOO_LARGE,
            false,
        ),
        (code!("task.too_many"), StatusCode::CONFLICT, false),
        (code!("task.no_room"), StatusCode::CONFLICT, false),
    ];
}

type Refused = http::Refused<Proposals>;

#[cfg(test)]
mod tests {
    use weacon_protocol::{ProposalChange, ProposalRecord, ProposalState, TaskStatus};

    use super::*;
    use crate::test_support::{
        self, Client, Fixture, error_code, member_id, request_with, status, workspace_id,
    };

    async fn decide(ada: &Client, proposal: &str, verdict: ProposalVerdict) -> (String, String) {
        let body = serde_json::to_string(&ProposalDecision { verdict }).unwrap();
        let head = format!("POST /api/workspaces/w1/proposals/{proposal}");
        ada.send(&head, &body).await
    }

    // m1's agent proposes a task called `title` as `proposal`.
    async fn proposed(fixture: &Fixture, proposal: &str, title: &str) {
        let workspace = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let change = ProposalChange::NewTask {
            title: title.to_owned(),
        };
        let proposed = test_support::proposals::proposed(proposal, "mc1", change);
        workspace.propose(&proposed).await.unwrap();
    }

    #[tokio::test]
    async fn a_member_lists_reads_and_decides_on_proposals() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        proposed(&fixture, "p1", "Ship").await;
        proposed(&fixture, "p2", "Test").await;
        let mut changes = fixture.shared.changes.subscribe();

        let (listed, body) = ada.send("GET /api/workspaces/w1/proposals", "").await;
        assert_eq!(listed, "http/1.1 200 ok");
        let list: ProposalList = serde_json::from_str(&body).unwrap();
        let ids: Vec<&str> = list
            .proposals
            .iter()
            .map(|record| record.id.as_str())
            .collect();
        assert_eq!(ids, ["p2", "p1"]);
        let (accepted, body) = decide(&ada, "p1", ProposalVerdict::Accept).await;
        assert_eq!(accepted, "http/1.1 200 ok");
        assert_eq!(
            serde_json::from_str::<ProposalOutcome>(&body).unwrap(),
            ProposalOutcome::Accepted { version: 1 }
        );
        let (rejected, body) = decide(&ada, "p2", ProposalVerdict::Reject).await;
        assert_eq!(rejected, "http/1.1 200 ok");
        assert_eq!(
            serde_json::from_str::<ProposalOutcome>(&body).unwrap(),
            ProposalOutcome::Rejected
        );
        let (read, body) = ada.send("GET /api/workspaces/w1/proposals/p2", "").await;
        assert_eq!(read, "http/1.1 200 ok");
        let record: ProposalRecord = serde_json::from_str(&body).unwrap();
        assert_eq!(
            record.state,
            ProposalState::Rejected {
                by: member_id("m1")
            }
        );
        let tasks = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let tasks = tasks.tasks().await.unwrap();
        assert_eq!((tasks.len(), tasks[0].status), (1, TaskStatus::Todo));
        assert_eq!(changes.try_recv().unwrap(), workspace_id("w1"));
        assert_eq!(changes.try_recv().unwrap(), workspace_id("w1"));
    }

    #[tokio::test]
    async fn a_decision_needs_an_open_proposal() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        proposed(&fixture, "p1", "Ship").await;
        decide(&ada, "p1", ProposalVerdict::Reject).await;

        for (head, body, answer, refused) in [
            (
                "POST /api/workspaces/w1/proposals/p1",
                r#"{"verdict":"accept"}"#,
                "http/1.1 409 conflict",
                "proposal.closed",
            ),
            (
                "POST /api/workspaces/w1/proposals/p9",
                r#"{"verdict":"accept"}"#,
                "http/1.1 404 not found",
                "proposal.not_found",
            ),
            (
                "POST /api/workspaces/w1/proposals/p1",
                r#"{"verdict":"defer"}"#,
                "http/1.1 400 bad request",
                "proposal.invalid",
            ),
            (
                "POST /api/workspaces/w1/proposals/p1",
                "{}",
                "http/1.1 400 bad request",
                "proposal.invalid",
            ),
            (
                "GET /api/workspaces/w1/proposals/p.1",
                "",
                "http/1.1 400 bad request",
                "proposal.invalid",
            ),
            (
                "GET /api/workspaces/w1/proposals/p9",
                "",
                "http/1.1 404 not found",
                "proposal.not_found",
            ),
        ] {
            let (answered, refusal_body) = ada.send(head, body).await;
            assert_eq!(
                (answered.as_str(), error_code(&refusal_body).as_str()),
                (answer, refused),
                "{head} {body}"
            );
        }
    }

    #[tokio::test]
    async fn only_a_member_sees_the_proposals() {
        let fixture = Fixture::new().await;
        let server = fixture.run().await;

        let path = "GET /api/workspaces/w1/proposals";
        let (head, _) = request_with(server.address, path, "", "").await;
        assert_eq!(status(&head), "http/1.1 401 unauthorized");
    }

    #[test]
    fn refusals_answer_with_their_status_and_nothing_else_is_a_failure() {
        let answer = |fault: Fault| {
            let response = axum::response::IntoResponse::into_response(Refused::from(fault));
            response.status().as_u16()
        };
        let expected = [
            ("proposal.invalid", 400),
            ("proposal.not_found", 404),
            ("proposal.closed", 409),
            ("doc.stale", 409),
            ("doc.too_large", 413),
            ("doc.quota_full", 413),
            ("task.too_many", 409),
            ("task.no_room", 409),
            ("db.failed", 500),
        ];
        let answered: Vec<(&str, u16)> = expected
            .iter()
            .map(|(refused, _)| {
                let code: Code = serde_json::from_value(serde_json::json!(refused)).unwrap();
                (*refused, answer(Fault::new(code, "Refused")))
            })
            .collect();

        assert_eq!(answered, expected);
    }
}
