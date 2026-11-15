//! Who is in a workspace: its owner invites and removes people, and an invited address, once
//! signed in, takes up its invitation.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use weacon_observability::Fault;
use weacon_protocol::{
    Code, Invitation, InvitationOffer, MemberAction, MemberCommand, WorkspaceId, code,
};

use crate::app::{fresh, new_id};
use crate::auth::{Member, Session};
use crate::db::{Account, InvitationRow, NewInvitation};
use crate::email::Email;
use crate::http::{Body, error, failure, invalid, not_found};
use crate::shared::Shared;
use crate::workspaces::name_of;

/// What the server offers to clients that manage who is in a workspace and hear it change.
pub(crate) const TEAM: Code = code!("workspace.team");

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/workspaces/{workspace}/accept", post(accept))
}

/// `POST /api/workspaces/{workspace}/members`: changes who is in the workspace, and tells its
/// feeds. An invitation is answered the same whether or not its address has an account.
pub(crate) async fn command(
    State(shared): State<Shared>,
    member: Member,
    Body(body): Body<MemberCommand>,
) -> Response {
    let workspace = &member.workspace;
    let done = match body.action {
        MemberAction::Invite { email } => {
            let Ok(email) = Email::parse(email.expose()) else {
                return invalid(code!("email.invalid"));
            };
            invite(&shared, &member, &email).await
        }
        MemberAction::Revoke { member: invited } => {
            workspace.revoke(&member.id, &invited, shared.now()).await
        }
        MemberAction::Remove { member: removed } => workspace.remove(&member.id, &removed).await,
        MemberAction::Leave => workspace.leave(&member.id).await,
        MemberAction::Newer => return invalid(code!("request.invalid")),
    };
    match done {
        Ok(()) => {
            shared.wake_feeds(workspace.id());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(fault) => refused(fault),
    }
}

async fn invite(shared: &Shared, member: &Member, email: &Email) -> Result<(), Fault> {
    let new = NewInvitation {
        member: &fresh(&new_id)?,
        by: &member.id,
        email,
        now: shared.now(),
    };
    member.workspace.invite(&new).await
}

/// `POST /api/workspaces/{workspace}/accept`: takes up the invitation to the signed-in account's
/// address, and answers with the workspace joined.
pub(crate) async fn accept(
    State(shared): State<Shared>,
    session: Session,
    Path(workspace): Path<WorkspaceId>,
) -> Response {
    let account = shared.db.account(session.account);
    match account.accept(&workspace, shared.now()).await {
        Ok(Some(summary)) => {
            shared.wake_feeds(&workspace);
            Json(summary).into_response()
        }
        Ok(None) => not_found(code!("invitation.not_found")),
        Err(fault) => failure(fault),
    }
}

/// The invitations `account` may take up, each with what its maker is called in its workspace.
pub(crate) async fn offers(
    shared: &Shared,
    account: &Account,
) -> Result<Vec<InvitationOffer>, Fault> {
    let mut offers = Vec::new();
    for offer in account.invitations(shared.now()).await? {
        let rows = shared
            .db
            .workspace(&offer.workspace)
            .await?
            .members()
            .await?;
        let invited_by = rows
            .iter()
            .find(|row| row.id == offer.invited_by)
            .map_or("", |row| name_of(&rows, row));
        offers.push(InvitationOffer {
            invited_by: invited_by.to_owned(),
            workspace: offer.workspace,
            name: offer.name,
            expires_at: offer.expires_at,
        });
    }
    Ok(offers)
}

pub(crate) fn invitation(row: InvitationRow) -> Invitation {
    Invitation {
        member: row.member,
        email: row.email.as_str().into(),
        expires_at: row.expires_at,
    }
}

fn refused(fault: Fault) -> Response {
    let code = fault.code().clone();
    if code == code!("member.not_allowed") {
        error(StatusCode::FORBIDDEN, code, false)
    } else if code == code!("member.not_found") {
        not_found(code)
    } else if code == code!("member.exists") || code == code!("invitation.exists") {
        error(StatusCode::CONFLICT, code, false)
    } else {
        failure(fault)
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use serde_json::{Value, json};
    use weacon_protocol::{Message, WorkspaceChange, WorkspaceUpdate};

    use crate::test_support::socket::{Socket, ends, next, open, say};
    use crate::test_support::{Client, Fixture, Running, ask, email, fresh_account, start_sign_in};

    // ada, signed in, and the workspace she made, Crew.
    async fn crew(fixture: &Fixture) -> (Running, Client, String) {
        let (server, ada) = fixture.client("ada@example.com").await;
        let create = json!({ "name": "Crew" }).to_string();
        let (_, made) = ask(ada.address, "POST /api/workspaces", &ada.proof(), &create).await;
        let id = made["id"].as_str().unwrap().to_owned();
        (server, ada, id)
    }

    async fn command(who: &Client, workspace: &str, action: Value) -> (String, Value) {
        let path = format!("POST /api/workspaces/{workspace}/members");
        let body = json!({ "action": action }).to_string();
        ask(who.address, &path, &who.proof(), &body).await
    }

    fn invite(address: &str) -> Value {
        json!({ "kind": "invite", "email": address })
    }

    async fn members(who: &Client, workspace: &str) -> Value {
        let path = format!("GET /api/workspaces/{workspace}/members");
        ask(who.address, &path, &who.proof(), "").await.1
    }

    async fn accept(who: &Client, workspace: &str) -> (String, Value) {
        let path = format!("POST /api/workspaces/{workspace}/accept");
        ask(who.address, &path, &who.proof(), "").await
    }

    async fn read(who: &Client, path: &str) -> (String, Value) {
        ask(who.address, &format!("GET {path}"), &who.proof(), "").await
    }

    async fn feed(address: SocketAddr, token: &str, workspace: &str, offers: &[&str]) -> Socket {
        let path = format!("/api/workspaces/{workspace}/feed");
        let mut socket = open(address, &path, token).await.unwrap();
        say(&mut socket, offers).await;
        assert!(matches!(next(&mut socket).await, Message::Welcome(_)));
        socket
    }

    async fn change(socket: &mut Socket) -> WorkspaceChange {
        let message = next(socket).await;
        let Message::Workspace(WorkspaceUpdate { change, .. }) = message else {
            panic!("{message:?}");
        };
        change
    }

    #[tokio::test]
    async fn an_invitation_is_answered_the_same_whoever_the_address_is() {
        let fixture = Fixture::new().await;
        let db = &fixture.shared.db;
        db.add_account(&fresh_account(), &email("cy@example.com"))
            .await
            .unwrap();
        let (_server, ada, crew) = crew(&fixture).await;

        let bo = command(&ada, &crew, invite("Bo@Example.com")).await;
        let cy = command(&ada, &crew, invite("cy@example.com")).await;
        let again = command(&ada, &crew, invite("bo@example.com")).await;
        let herself = command(&ada, &crew, invite("ada@example.com")).await;
        let bad = command(&ada, &crew, invite("not an address")).await;
        let newer = command(&ada, &crew, json!({ "kind": "transfer" })).await;

        assert_eq!(bo, ("http/1.1 204 no content".to_owned(), Value::Null));
        assert_eq!(cy, bo);
        assert_eq!(again.0, "http/1.1 409 conflict");
        assert_eq!(again.1["code"], "invitation.exists");
        assert_eq!(herself.1["code"], "member.exists");
        assert_eq!(bad.1["code"], "email.invalid");
        assert_eq!(newer.1["code"], "request.invalid");
        let listed = members(&ada, &crew).await;
        let invited: Vec<_> = listed["invitations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|invitation| invitation["email"].clone())
            .collect();
        assert_eq!(invited, [json!("bo@example.com"), json!("cy@example.com")]);
        assert_eq!(listed["members"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_invited_address_signs_in_takes_it_up_and_the_workspace_hears_it() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        let offers = ["workspace.feed", "workspace.team"];
        let mut ada_feed = feed(server.address, &ada.token, &crew, &offers).await;
        let joined = change(&mut ada_feed).await;
        assert!(matches!(joined, WorkspaceChange::MemberJoined { .. }));

        command(&ada, &crew, invite("bo@example.com")).await;
        assert_eq!(change(&mut ada_feed).await, WorkspaceChange::MembersChanged);
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;
        let outside = read(&bo, &format!("/api/workspaces/{crew}")).await;
        let (taken, summary) = accept(&bo, &crew).await;
        let (_, after) = read(&bo, "/api/workspaces").await;
        let inside = read(&bo, &format!("/api/workspaces/{crew}")).await;

        assert_eq!(outside.0, "http/1.1 401 unauthorized");
        assert_eq!(taken, "http/1.1 200 ok");
        assert_eq!(summary["role"], "member");
        assert!(after["workspaces"].as_array().unwrap().contains(&summary));
        assert_eq!(after["invitations"], json!([]));
        assert_eq!(inside.0, "http/1.1 200 ok");
        assert_eq!(change(&mut ada_feed).await, WorkspaceChange::MembersChanged);
        assert!(members(&bo, &crew).await.get("invitations").is_none());
        assert_eq!(accept(&bo, &crew).await.0, "http/1.1 404 not found");
    }

    #[tokio::test]
    async fn an_invited_address_sees_the_workspace_and_who_invited_it() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com")).await;
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;

        let (_, listed) = read(&bo, "/api/workspaces").await;

        assert_eq!(listed["workspaces"][0]["name"], "Personal");
        let offers = listed["invitations"].as_array().unwrap();
        assert_eq!(offers.len(), 1);
        let offer = &offers[0];
        let seen = [&offer["workspace"], &offer["name"], &offer["invited_by"]];
        assert_eq!(seen, [&json!(crew), &json!("Crew"), &json!("ada")]);
    }

    #[tokio::test]
    async fn only_an_invitation_gets_an_address_a_code_where_nobody_may_sign_up() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com")).await;

        start_sign_in(server.address, "di@example.com").await;
        fixture.settled().await;
        assert!(fixture.letters().is_empty());
        start_sign_in(server.address, "bo@example.com").await;
        let (to, code) = fixture.letter().await;
        assert_eq!((to.as_str(), code.len()), ("bo@example.com", 9));
    }

    #[tokio::test]
    async fn a_removed_member_is_refused_at_once_and_their_feed_ends() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com")).await;
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;
        let (_, summary) = accept(&bo, &crew).await;
        let member = summary["member"].clone();
        // A feed that hears of no members' changes ends all the same.
        let mut bo_feed = feed(server.address, &bo.token, &crew, &["workspace.feed"]).await;
        let joined = change(&mut bo_feed).await;
        assert!(matches!(joined, WorkspaceChange::MemberJoined { .. }));

        let by_member = command(&bo, &crew, invite("cy@example.com")).await;
        let removed = command(&ada, &crew, json!({ "kind": "remove", "member": member })).await;
        let nobody = command(&ada, &crew, json!({ "kind": "remove", "member": "m9" })).await;

        assert_eq!(by_member.0, "http/1.1 403 forbidden");
        assert_eq!(by_member.1["code"], "member.not_allowed");
        assert_eq!(removed.0, "http/1.1 204 no content");
        assert_eq!(nobody.1["code"], "member.not_found");
        assert!(ends(&mut bo_feed).await);
        let refused = read(&bo, &format!("/api/workspaces/{crew}")).await;
        assert_eq!(refused.0, "http/1.1 401 unauthorized");
        let left = command(&bo, &crew, json!({ "kind": "leave" })).await;
        assert_eq!(left.0, "http/1.1 401 unauthorized");
        let past = &members(&ada, &crew).await["members"][1];
        assert_eq!(past, &json!({ "id": member, "name": "bo" }));
    }

    #[tokio::test]
    async fn a_revoked_invitation_cant_be_taken_up() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com")).await;
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;
        let invitation = members(&ada, &crew).await["invitations"][0]["member"].clone();

        let revoke = json!({ "kind": "revoke", "member": invitation });
        assert_eq!(
            command(&ada, &crew, revoke).await.0,
            "http/1.1 204 no content"
        );
        assert_eq!(accept(&bo, &crew).await.0, "http/1.1 404 not found");
        assert_eq!(accept(&bo, "w1").await.0, "http/1.1 404 not found");
    }
}
