use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use weacon_model::Title;
use weacon_observability::Fault;
use weacon_protocol::{WorkspaceCreate, WorkspaceList, WorkspaceSummary, code};

use crate::app::{fresh, new_id};
use crate::auth::{Member, Session};
use crate::db::NewWorkspace;
use crate::http::{Body, failure, invalid, unauthorized};
use crate::shared::Shared;

pub(crate) const MAX_NAME: usize = 100;
/// What an account's own workspace is called when it is made.
const PERSONAL: &str = "Personal";

/// `POST /api/workspaces`: makes a workspace that the signed-in account owns.
pub(crate) async fn create(
    State(shared): State<Shared>,
    session: Session,
    Body(body): Body<WorkspaceCreate>,
) -> Response {
    let made = match workspace_name(&body.name).and_then(new_workspace) {
        Ok(new) => {
            shared
                .db
                .account(session.account)
                .create_workspace(new)
                .await
        }
        Err(fault) => return invalid(fault.code().clone()),
    };
    created(made)
}

fn created(made: Result<WorkspaceSummary, Fault>) -> Response {
    match made {
        Ok(summary) => (StatusCode::CREATED, Json(summary)).into_response(),
        Err(fault) if *fault.code() == code!("workspace.too_many") => invalid(fault.code().clone()),
        Err(fault) => failure(fault),
    }
}

/// `GET /api/workspaces`: the signed-in account's workspaces.
pub(crate) async fn list(State(shared): State<Shared>, session: Session) -> Response {
    match shared.db.account(session.account).workspaces().await {
        Ok(workspaces) => Json(WorkspaceList { workspaces }).into_response(),
        Err(fault) => failure(fault),
    }
}

/// `GET /api/workspaces/{workspace}`: one workspace, to one of its members.
pub(crate) async fn read(member: Member) -> Response {
    match member.workspace.summary(&member.id).await {
        Ok(Some(summary)) => Json(summary).into_response(),
        // Someone else ended the membership since it was checked.
        Ok(None) => unauthorized(code!("sign_in.required")),
        Err(fault) => failure(fault),
    }
}

/// The Personal workspace a sign-in makes, if its account has never had one.
pub(crate) fn personal() -> Result<NewWorkspace, Fault> {
    new_workspace(PERSONAL)
}

fn new_workspace(name: &str) -> Result<NewWorkspace, Fault> {
    Ok(NewWorkspace {
        id: fresh(&new_id)?,
        name: name.to_owned(),
        owner: fresh(&new_id)?,
    })
}

/// `name` trimmed, if it is a title of at most [`MAX_NAME`] characters.
pub(crate) fn workspace_name(name: &str) -> Result<&str, Fault> {
    let name = name.trim();
    let fits = name.chars().count() <= MAX_NAME && Title::try_from(name.to_owned()).is_ok();
    fits.then_some(name)
        .ok_or_else(|| Fault::new(code!("workspace.name_invalid"), "That name can't be used"))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{
        Client, Fixture, ask, email, fresh_account, header, request_with, status,
    };

    async fn listed(who: &Client) -> Vec<Value> {
        let (_, list) = ask(who.address, "GET /api/workspaces", &who.auth, "").await;
        list["workspaces"].as_array().unwrap().clone()
    }

    #[tokio::test]
    async fn a_person_makes_a_workspace_they_own_and_finds_it_listed() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let create = json!({ "name": "  Team two " }).to_string();

        let (created, made) = ask(ada.address, "POST /api/workspaces", &ada.auth, &create).await;
        let id = made["id"].as_str().unwrap();
        let read = ask(
            ada.address,
            &format!("GET /api/workspaces/{id}"),
            &ada.auth,
            "",
        )
        .await;
        let workspaces = listed(&ada).await;

        assert_eq!(created, "http/1.1 201 created");
        assert_eq!(made["name"], "Team two");
        assert_eq!(made["role"], "owner");
        assert_eq!(id.len(), 32);
        assert_eq!(read, ("http/1.1 200 ok".to_owned(), made.clone()));
        let names: Vec<_> = workspaces.iter().map(|seen| seen["name"].clone()).collect();
        let roles: Vec<_> = workspaces.iter().map(|seen| seen["role"].clone()).collect();
        assert_eq!(names, [json!("Team"), json!("Personal"), json!("Team two")]);
        assert_eq!(roles, [json!("member"), json!("owner"), json!("owner")]);
    }

    #[tokio::test]
    async fn signing_in_again_makes_no_second_personal_workspace() {
        let fixture = Fixture::new().await;
        drop(fixture.client("ada@example.com").await);
        let (_server, ada) = fixture.client("ada@example.com").await;

        let personal = listed(&ada)
            .await
            .into_iter()
            .filter(|seen| seen["name"] == "Personal")
            .count();

        assert_eq!(personal, 1);
    }

    #[tokio::test]
    async fn what_an_account_sees_is_kept_out_of_caches() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let mut kept = Vec::new();
        for path in ["/api/workspaces", "/api/workspaces/w1", "/api/session"] {
            let (head, _) = request_with(ada.address, &format!("GET {path}"), &ada.auth, "").await;
            assert_eq!(status(&head), "http/1.1 200 ok", "{path}");
            kept.push(
                header(&head, "cache-control")
                    .unwrap_or_default()
                    .to_owned(),
            );
        }

        assert_eq!(kept, ["no-store"; 3]);
    }

    #[tokio::test]
    async fn nobody_reads_a_workspace_they_arent_in() {
        let fixture = Fixture::new().await;
        let bo = email("bo@example.com");
        fixture
            .shared
            .db
            .add_account(&fresh_account(), &bo)
            .await
            .unwrap();
        let (_server, ada) = fixture.client("ada@example.com").await;
        let (_server, bo) = fixture.client("bo@example.com").await;
        let theirs = listed(&ada).await;
        let mine = listed(&bo).await;
        let required = json!({ "code": "sign_in.required", "retryable": false });
        let mut refusals = Vec::new();
        for id in theirs
            .iter()
            .map(|seen| seen["id"].as_str().unwrap())
            .chain(["w9"])
        {
            let read = format!("GET /api/workspaces/{id}");
            refusals.push(ask(bo.address, &read, &bo.auth, "").await);
            refusals.push(ask(ada.address, &read, "", "").await);
        }
        let bad_id = ask(ada.address, "GET /api/workspaces/w.1", &ada.auth, "").await;
        let (nobody, _) = ask(ada.address, "GET /api/workspaces", "", "").await;

        assert_eq!(theirs.len(), 2);
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0]["name"], "Personal");
        assert!(!theirs.contains(&mine[0]));
        assert_eq!(refusals.len(), 6);
        for refusal in refusals {
            assert_eq!(
                refusal,
                ("http/1.1 401 unauthorized".to_owned(), required.clone())
            );
        }
        assert_eq!(
            bad_id,
            (
                "http/1.1 400 bad request".to_owned(),
                json!({ "code": "request.invalid", "retryable": false })
            )
        );
        assert_eq!(nobody, "http/1.1 401 unauthorized");
    }

    #[tokio::test]
    async fn an_account_that_owns_too_many_is_told_so() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let create = json!({ "name": "Team" }).to_string();
        // The Personal workspace is one of them.
        for _ in 1..100 {
            let (made, _) = ada.send("POST /api/workspaces", &create).await;
            assert_eq!(made, "http/1.1 201 created");
        }

        let more = ask(ada.address, "POST /api/workspaces", &ada.auth, &create).await;

        let refused = json!({ "code": "workspace.too_many", "retryable": false });
        assert_eq!(
            more,
            ("http/1.1 422 unprocessable entity".to_owned(), refused)
        );
    }

    #[tokio::test]
    async fn a_workspace_needs_a_name_of_one_short_line_that_keeps_its_direction_and_a_session() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let refused = json!({ "code": "workspace.name_invalid", "retryable": false });
        let long = "a".repeat(MAX_NAME + 1);
        let refused_names = [
            "",
            "   ",
            "a\nb",
            "a\u{2028}b",
            "a\u{2029}b",
            "a\u{202A}b",
            "a\u{202E}b",
            "a\u{2066}b",
            "a\u{2069}b",
            long.as_str(),
        ];
        for name in refused_names {
            let create = json!({ "name": name }).to_string();
            let answer = ask(ada.address, "POST /api/workspaces", &ada.auth, &create).await;
            assert_eq!(
                answer,
                (
                    "http/1.1 422 unprocessable entity".to_owned(),
                    refused.clone()
                ),
                "{name:?}"
            );
        }
        let create = json!({ "name": "Team" }).to_string();
        let (nobody, _) = ask(ada.address, "POST /api/workspaces", "", &create).await;
        let page = "cookie: __Host-weacon_session=x\r\norigin: https://other.example\r\n";
        let (crossed, _) = ask(ada.address, "POST /api/workspaces", page, &create).await;

        assert_eq!(listed(&ada).await.len(), 2);
        assert_eq!(nobody, "http/1.1 401 unauthorized");
        assert_eq!(crossed, "http/1.1 403 forbidden");
        Title::try_from(long.clone()).unwrap();
        assert_eq!(workspace_name(" Team\u{200F} ").unwrap(), "Team\u{200F}");
        assert_eq!(
            workspace_name(&"é".repeat(MAX_NAME))
                .unwrap()
                .chars()
                .count(),
            MAX_NAME
        );
    }

    #[test]
    fn only_a_full_account_is_refused_and_other_faults_fail() {
        let full = created(Err(Fault::new(code!("workspace.too_many"), "Full")));
        let broken = created(Err(Fault::new(code!("db.failed"), "Broken")));

        assert_eq!(full.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(broken.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
