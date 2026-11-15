//! Who is in a workspace: its owner invites and removes people, and an invited address, once
//! signed in, takes up its invitation.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use weacon_protocol::{Code, Invitation, MemberAction, MemberCommand, WorkspaceId, code};

use crate::app::{fresh, new_id};
use crate::auth::{Member, Session};
use crate::db::InvitationRow;
use crate::email::Email;
use crate::http::{Body, Refusals, Refused, failure, invalid, not_found};
use crate::shared::Shared;

struct Team;

impl Refusals for Team {
    const REFUSALS: &'static [(Code, StatusCode, bool)] = &[
        (code!("member.not_allowed"), StatusCode::FORBIDDEN, false),
        (code!("member.not_found"), StatusCode::NOT_FOUND, false),
        (code!("member.exists"), StatusCode::CONFLICT, false),
        (code!("invitation.exists"), StatusCode::CONFLICT, false),
    ];
}

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
    let (workspace, by, now) = (&member.workspace, &member.id, shared.now());
    let done = match body.action {
        MemberAction::Invite { email } => match Email::parse(email.expose()) {
            Ok(email) => workspace.invite(by, &email, now).await,
            Err(_) => return invalid(code!("email.invalid")),
        },
        MemberAction::Revoke { email } => match Email::parse(email.expose()) {
            Ok(email) => workspace.revoke(by, &email, now).await,
            Err(_) => return invalid(code!("email.invalid")),
        },
        MemberAction::Remove { member: removed } => workspace.remove(by, &removed).await,
        MemberAction::Leave => workspace.leave(by).await,
    };
    match done {
        Ok(()) => {
            shared.wake_feeds(workspace.id());
            StatusCode::NO_CONTENT.into_response()
        }
        Err(fault) => Refused::<Team>::from(fault).into_response(),
    }
}

/// `POST /api/workspaces/{workspace}/accept`: takes up the invitation to the signed-in account's
/// address, and answers with the workspace joined.
pub(crate) async fn accept(
    State(shared): State<Shared>,
    session: Session,
    Path(workspace): Path<WorkspaceId>,
) -> Response {
    let accepted = async {
        let member = fresh(&new_id)?;
        let account = shared.db.account(session.account);
        account.accept(&workspace, &member, shared.now()).await
    };
    match accepted.await {
        Ok(Some(summary)) => {
            shared.wake_feeds(&workspace);
            Json(summary).into_response()
        }
        Ok(None) => not_found(code!("invitation.not_found")),
        Err(fault) => failure(fault),
    }
}

impl From<InvitationRow> for Invitation {
    fn from(row: InvitationRow) -> Self {
        Self {
            email: row.email.as_str().into(),
            expires_at: row.expires_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use serde_json::{Value, json};
    use weacon_protocol::{Message, WorkspaceChange, WorkspaceUpdate};

    use crate::test_support::socket::{Socket, next, open, say};
    use crate::test_support::{Client, Fixture, Running, ask, email, fresh_account};

    // ada, signed in, and the workspace she made, Crew.
    async fn crew(fixture: &Fixture) -> (Running, Client, String) {
        let (server, ada) = fixture.client("ada@example.com").await;
        let create = json!({ "name": "Crew" }).to_string();
        let (_, made) = ask(ada.address, "POST /api/workspaces", &ada.proof(), &create).await;
        let id = made["id"].as_str().unwrap().to_owned();
        (server, ada, id)
    }

    // `address`, with an account, signed in.
    async fn signed_in(fixture: &Fixture, address: SocketAddr, email_address: &str) -> Client {
        let db = &fixture.shared.db;
        db.add_account(&fresh_account(), &email(email_address))
            .await
            .unwrap();
        fixture.sign_in_client(address, email_address).await
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
        let offers = ["workspace.feed"];
        let mut ada_feed = feed(server.address, &ada.token, &crew, &offers).await;
        let joined = change(&mut ada_feed).await;
        assert!(matches!(joined, WorkspaceChange::MemberJoined { .. }));

        command(&ada, &crew, invite("bo@example.com")).await;
        assert_eq!(change(&mut ada_feed).await, WorkspaceChange::MembersChanged);
        let bo = signed_in(&fixture, server.address, "bo@example.com").await;
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
    async fn an_invited_address_sees_the_workspace() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com")).await;
        let bo = signed_in(&fixture, server.address, "bo@example.com").await;

        let (_, listed) = read(&bo, "/api/workspaces").await;

        assert_eq!(listed["workspaces"][0]["name"], "Personal");
        let offers = listed["invitations"].as_array().unwrap();
        assert_eq!(offers.len(), 1);
        let offer = &offers[0];
        let seen = [&offer["workspace"], &offer["name"]];
        assert_eq!(seen, [&json!(crew), &json!("Crew")]);
    }

    #[tokio::test]
    async fn a_removed_member_is_refused_at_once() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com")).await;
        let bo = signed_in(&fixture, server.address, "bo@example.com").await;
        let (_, summary) = accept(&bo, &crew).await;
        let member = summary["member"].clone();

        let by_member = command(&bo, &crew, invite("cy@example.com")).await;
        let removed = command(&ada, &crew, json!({ "kind": "remove", "member": member })).await;
        let nobody = command(&ada, &crew, json!({ "kind": "remove", "member": "m9" })).await;

        assert_eq!(by_member.0, "http/1.1 403 forbidden");
        assert_eq!(by_member.1["code"], "member.not_allowed");
        assert_eq!(removed.0, "http/1.1 204 no content");
        assert_eq!(nobody.1["code"], "member.not_found");
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
        let bo = signed_in(&fixture, server.address, "bo@example.com").await;
        let invitation = members(&ada, &crew).await["invitations"][0]["email"].clone();

        let revoke = json!({ "kind": "revoke", "email": invitation });
        assert_eq!(
            command(&ada, &crew, revoke).await.0,
            "http/1.1 204 no content"
        );
        assert_eq!(accept(&bo, &crew).await.0, "http/1.1 404 not found");
        assert_eq!(accept(&bo, "w1").await.0, "http/1.1 404 not found");
    }
}
