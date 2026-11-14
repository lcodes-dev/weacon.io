//! Agents' proposals to docs and tasks, which members accept or reject.

use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use weacon_observability::Fault;
use weacon_protocol::{
    Code, ProposalDecision, ProposalId, ProposalList, ProposalOutcome, ProposalVerdict,
    WorkspaceId, code,
};

use crate::auth::Member;
use crate::http::{Body, Shared, Unreadable, error, failure};

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
async fn list(member: Member) -> Response {
    match member.workspace.proposals().await {
        Ok(proposals) => Json(ProposalList { proposals }).into_response(),
        Err(fault) => answer(fault),
    }
}

/// `GET /api/workspaces/{workspace}/proposals/{proposal}`: one proposal.
async fn read(
    member: Member,
    path: Result<Path<(WorkspaceId, ProposalId)>, PathRejection>,
) -> Response {
    let found = async {
        let Path((_, id)) = path.map_err(|_| invalid())?;
        member.workspace.proposal(&id).await
    };
    match found.await {
        Ok(record) => Json(record).into_response(),
        Err(fault) => answer(fault),
    }
}

/// `POST /api/workspaces/{workspace}/proposals/{proposal}`: a member accepts or rejects an
/// open proposal. Every member edits every doc and task, so any member may decide.
async fn decide(
    State(shared): State<Shared>,
    member: Member,
    path: Result<Path<(WorkspaceId, ProposalId)>, PathRejection>,
    body: Result<Body<ProposalDecision>, Unreadable>,
) -> Response {
    let decided = async {
        let Path((_, id)) = path.map_err(|_| invalid())?;
        let Body(decision) = body.map_err(|_| invalid())?;
        let workspace = &member.workspace;
        match decision.verdict {
            ProposalVerdict::Accept => workspace.accept(&id, &member.id, shared.now()).await,
            ProposalVerdict::Reject => workspace
                .reject(&id, &member.id)
                .await
                .map(|()| ProposalOutcome::Rejected),
            ProposalVerdict::Newer => Err(invalid()),
        }
    };
    match decided.await {
        Ok(outcome) => {
            if matches!(
                outcome,
                ProposalOutcome::Accepted { .. } | ProposalOutcome::Rejected
            ) {
                shared.changed(member.workspace.id());
            }
            Json(outcome).into_response()
        }
        Err(fault) => answer(fault),
    }
}

fn invalid() -> Fault {
    Fault::new(
        code!("proposal.invalid"),
        "The proposal request isn't valid",
    )
}

const REFUSALS: [(Code, StatusCode); 6] = [
    (code!("proposal.invalid"), StatusCode::BAD_REQUEST),
    (code!("proposal.not_found"), StatusCode::NOT_FOUND),
    (code!("proposal.closed"), StatusCode::CONFLICT),
    (code!("doc.quota_full"), StatusCode::PAYLOAD_TOO_LARGE),
    (code!("task.too_many"), StatusCode::CONFLICT),
    (code!("task.no_room"), StatusCode::CONFLICT),
];

/// Answers a refusal with its status, and anything else as the server failing.
fn answer(fault: Fault) -> Response {
    let code = fault.code().clone();
    match REFUSALS.iter().find(|(refusal, _)| *refusal == code) {
        Some((_, status)) => error(*status, code, false),
        None => failure(fault),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use weacon_protocol::{ProposalChange, ProposalRecord, ProposalState, TaskStatus};

    use super::*;
    use crate::test_support::{
        self, Fixture, Running, bearer, member_id, refusal, request_with, status, workspace_id,
    };

    struct Ada {
        address: SocketAddr,
        auth: String,
    }

    impl Ada {
        async fn send(&self, head: &str, body: &str) -> (String, String) {
            let (head, body) = request_with(self.address, head, &self.auth, body).await;
            (status(&head).to_owned(), body)
        }

        async fn decide(&self, proposal: &str, verdict: ProposalVerdict) -> (String, String) {
            let body = serde_json::to_string(&ProposalDecision { verdict }).unwrap();
            let head = format!("POST /api/workspaces/w1/proposals/{proposal}");
            self.send(&head, &body).await
        }
    }

    async fn ada(fixture: &Fixture) -> (Running, Ada) {
        let (server, body) = fixture.signed_in("ada@example.com").await;
        let ada = Ada {
            address: server.address,
            auth: bearer(&body),
        };
        (server, ada)
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
        let (_server, ada) = ada(&fixture).await;
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
        let (accepted, body) = ada.decide("p1", ProposalVerdict::Accept).await;
        assert_eq!(accepted, "http/1.1 200 ok");
        assert_eq!(
            serde_json::from_str::<ProposalOutcome>(&body).unwrap(),
            ProposalOutcome::Accepted { version: 1 }
        );
        let (rejected, body) = ada.decide("p2", ProposalVerdict::Reject).await;
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
        let (_server, ada) = ada(&fixture).await;
        proposed(&fixture, "p1", "Ship").await;
        ada.decide("p1", ProposalVerdict::Reject).await;

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
                (answered.as_str(), refusal(&refusal_body).as_str()),
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
}
