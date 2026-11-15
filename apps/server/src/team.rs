//! Who is in a workspace: its owner and admins invite and remove people, and an invited
//! address, once signed in, takes up or hides its invitation.

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use weacon_observability::{Fault, Field};
use weacon_protocol::{
    Code, Invitation, InvitationAction, InvitationCommand, InvitationOffer, MemberAction,
    MemberCommand, code,
};

use crate::app::{fresh, new_id};
use crate::auth::{Member, Session};
use crate::db::{Account, InvitationRow, NewInvitation};
use crate::email::Email;
use crate::http::{Body, error, failure, invalid, not_found, too_many};
use crate::outbox::mail_failed;
use crate::shared::Shared;
use crate::workspaces::name_of;

/// What the server offers to clients that manage who is in a workspace and hear it change.
pub(crate) const TEAM: Code = code!("workspace.team");

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/invitations", post(answer))
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
        MemberAction::Invite { email, role } => {
            let Ok(email) = Email::parse(email.expose()) else {
                return invalid(code!("email.invalid"));
            };
            invite(&shared, &member, &email, role).await
        }
        MemberAction::Revoke { member: invited } => {
            workspace.revoke(&member.id, &invited, shared.now()).await
        }
        MemberAction::Remove { member: removed } => workspace.remove(&member.id, &removed).await,
        MemberAction::SetRole {
            member: other,
            role,
        } => workspace.set_role(&member.id, &other, role).await,
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

async fn invite(
    shared: &Shared,
    member: &Member,
    email: &Email,
    role: weacon_protocol::WorkspaceRole,
) -> Result<(), Fault> {
    let new = NewInvitation {
        member: &fresh(&new_id)?,
        by: &member.id,
        email,
        role,
        now: shared.now(),
    };
    if member.workspace.invite(&new).await? {
        let (outbox, to) = (shared.outbox.clone(), email.clone());
        let left = tokio::task::spawn_blocking(move || outbox.notify(&to)).await;
        // The invitation stands whether or not its notice is left.
        if let Err(fault) = left
            .map_err(|error| mail_failed().because(error))
            .and_then(|left| left)
        {
            let workspace = member.workspace.id().as_str().into();
            let fields = [("workspace_id", workspace), ("err", Field::error(&fault))];
            shared.log.warn("notice failed", &fields);
        }
    }
    Ok(())
}

/// `POST /api/invitations`: takes up or hides an invitation to the signed-in account's address.
pub(crate) async fn answer(
    State(shared): State<Shared>,
    session: Session,
    Body(body): Body<InvitationCommand>,
) -> Response {
    let account = shared.db.account(session.account);
    let now = shared.now();
    match body.action {
        InvitationAction::Accept => match account.accept(&body.workspace, now).await {
            Ok(Some(summary)) => {
                shared.wake_feeds(&body.workspace);
                Json(summary).into_response()
            }
            Ok(None) => not_found(code!("invitation.not_found")),
            Err(fault) => failure(fault),
        },
        InvitationAction::Hide => match account.hide(&body.workspace, now).await {
            Ok(true) => StatusCode::NO_CONTENT.into_response(),
            Ok(false) => not_found(code!("invitation.not_found")),
            Err(fault) => failure(fault),
        },
        InvitationAction::Newer => invalid(code!("request.invalid")),
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
            role: offer.role,
            expires_at: offer.expires_at,
        });
    }
    Ok(offers)
}

pub(crate) fn invitation(row: InvitationRow) -> Invitation {
    Invitation {
        member: row.member,
        email: row.email.as_str().into(),
        role: row.role,
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
    } else if code == code!("invitation.too_many") {
        too_many(code)
    } else {
        failure(fault)
    }
}

#[cfg(test)]
mod tests {
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

    fn invite(address: &str, role: &str) -> Value {
        json!({ "kind": "invite", "email": address, "role": role })
    }

    async fn members(who: &Client, workspace: &str) -> Value {
        let path = format!("GET /api/workspaces/{workspace}/members");
        ask(who.address, &path, &who.proof(), "").await.1
    }

    async fn answer(who: &Client, workspace: &str, kind: &str) -> (String, Value) {
        let body = json!({ "workspace": workspace, "action": { "kind": kind } }).to_string();
        ask(who.address, "POST /api/invitations", &who.proof(), &body).await
    }

    // Every letter waiting, each taken out of the outbox.
    fn letters(fixture: &Fixture) -> Vec<Value> {
        fixture
            .letters()
            .into_iter()
            .map(|file| {
                let letter = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
                std::fs::remove_file(file).unwrap();
                letter
            })
            .collect()
    }

    async fn feed(
        address: std::net::SocketAddr,
        token: &str,
        workspace: &str,
        offers: &[&str],
    ) -> Socket {
        let mut socket = open(address, &format!("/api/workspaces/{workspace}/feed"), token)
            .await
            .unwrap();
        say(&mut socket, offers).await;
        assert!(matches!(next(&mut socket).await, Message::Welcome(_)));
        socket
    }

    fn change(message: Message) -> WorkspaceChange {
        let Message::Workspace(WorkspaceUpdate { change, .. }) = message else {
            panic!("{message:?}");
        };
        change
    }

    #[tokio::test]
    async fn an_invitation_is_answered_the_same_whoever_the_address_is_and_notices_say_nothing_else()
     {
        let fixture = Fixture::new().await;
        let db = &fixture.shared.db;
        db.add_account(&fresh_account(), &email("cy@example.com"))
            .await
            .unwrap();
        let (_server, ada, crew) = crew(&fixture).await;

        let bo = command(&ada, &crew, invite("Bo@Example.com", "member")).await;
        let cy = command(&ada, &crew, invite("cy@example.com", "admin")).await;
        let again = command(&ada, &crew, invite("bo@example.com", "admin")).await;
        let herself = command(&ada, &crew, invite("ada@example.com", "member")).await;

        assert_eq!(bo, ("http/1.1 204 no content".to_owned(), Value::Null));
        assert_eq!(cy, bo);
        assert_eq!(again.0, "http/1.1 409 conflict");
        assert_eq!(again.1["code"], "invitation.exists");
        assert_eq!(herself.1["code"], "member.exists");
        let mut told = letters(&fixture);
        told.sort_by_key(|letter| letter["to"].to_string());
        assert_eq!(
            told,
            [
                json!({ "to": "bo@example.com", "notice": { "kind": "invited" } }),
                json!({ "to": "cy@example.com", "notice": { "kind": "invited" } })
            ]
        );
        let listed = members(&ada, &crew).await;
        let invited: Vec<_> = listed["invitations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|invitation| (invitation["email"].clone(), invitation["role"].clone()))
            .collect();
        assert_eq!(
            invited,
            [
                (json!("bo@example.com"), json!("member")),
                (json!("cy@example.com"), json!("admin"))
            ]
        );
        assert_eq!(listed["members"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_invited_address_signs_in_takes_it_up_and_the_workspace_hears_it() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        let mut ada_feed = feed(
            server.address,
            &ada.token,
            &crew,
            &["workspace.feed", "workspace.team"],
        )
        .await;
        assert!(matches!(
            change(next(&mut ada_feed).await),
            WorkspaceChange::MemberJoined { .. }
        ));

        command(&ada, &crew, invite("bo@example.com", "member")).await;
        assert_eq!(
            change(next(&mut ada_feed).await),
            WorkspaceChange::MembersChanged
        );
        letters(&fixture);
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;
        let outside = ask(
            bo.address,
            &format!("GET /api/workspaces/{crew}"),
            &bo.proof(),
            "",
        )
        .await;
        let (joined, summary) = answer(&bo, &crew, "accept").await;
        let (_, after) = ask(bo.address, "GET /api/workspaces", &bo.proof(), "").await;
        let inside = ask(
            bo.address,
            &format!("GET /api/workspaces/{crew}"),
            &bo.proof(),
            "",
        )
        .await;

        assert_eq!(outside.0, "http/1.1 401 unauthorized");
        assert_eq!(joined, "http/1.1 200 ok");
        assert_eq!(summary["role"], "member");
        assert_eq!(after["workspaces"], json!([summary]));
        assert_eq!(after["invitations"], json!([]));
        assert_eq!(inside.0, "http/1.1 200 ok");
        assert_eq!(
            change(next(&mut ada_feed).await),
            WorkspaceChange::MembersChanged
        );
        assert!(members(&bo, &crew).await.get("invitations").is_none());
    }

    #[tokio::test]
    async fn an_invited_address_sees_the_workspace_and_who_invited_it() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com", "member")).await;
        letters(&fixture);
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;

        let (_, listed) = ask(bo.address, "GET /api/workspaces", &bo.proof(), "").await;

        assert_eq!(listed["workspaces"], json!([]));
        let offers = listed["invitations"].as_array().unwrap();
        assert_eq!(offers.len(), 1);
        let offer = &offers[0];
        let seen = [
            &offer["workspace"],
            &offer["name"],
            &offer["invited_by"],
            &offer["role"],
        ];
        assert_eq!(
            seen,
            [
                &json!(crew),
                &json!("Crew"),
                &json!("ada"),
                &json!("member")
            ]
        );
    }

    #[tokio::test]
    async fn only_an_invitation_gets_an_address_a_code_where_nobody_may_sign_up() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com", "member")).await;
        letters(&fixture);

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
        command(&ada, &crew, invite("bo@example.com", "member")).await;
        letters(&fixture);
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;
        let (_, summary) = answer(&bo, &crew, "accept").await;
        let member = summary["member"].clone();
        // A feed that hears of no members' changes ends all the same.
        let mut bo_feed = feed(server.address, &bo.token, &crew, &["workspace.feed"]).await;
        assert!(matches!(
            change(next(&mut bo_feed).await),
            WorkspaceChange::MemberJoined { .. }
        ));

        let by_member = command(&bo, &crew, invite("cy@example.com", "member")).await;
        let made_admin = command(
            &ada,
            &crew,
            json!({ "kind": "set_role", "member": member, "role": "admin" }),
        )
        .await;
        let removed = command(&ada, &crew, json!({ "kind": "remove", "member": member })).await;

        assert_eq!(by_member.0, "http/1.1 403 forbidden");
        assert_eq!(by_member.1["code"], "member.not_allowed");
        assert_eq!(made_admin.0, "http/1.1 204 no content");
        assert_eq!(removed.0, "http/1.1 204 no content");
        assert!(ends(&mut bo_feed).await);
        let read = ask(
            bo.address,
            &format!("GET /api/workspaces/{crew}"),
            &bo.proof(),
            "",
        )
        .await;
        assert_eq!(read.0, "http/1.1 401 unauthorized");
        let left = command(&bo, &crew, json!({ "kind": "leave" })).await;
        assert_eq!(left.0, "http/1.1 401 unauthorized");
        let (_, listed) = ask(bo.address, "GET /api/workspaces", &bo.proof(), "").await;
        assert_eq!(listed["workspaces"], json!([]));
        let past = &members(&ada, &crew).await["members"][1];
        assert_eq!(past, &json!({ "id": member, "name": "bo" }));
    }

    #[tokio::test]
    async fn a_hidden_invitation_reads_as_live_to_the_workspace_and_odd_asks_are_refused() {
        let fixture = Fixture::new().await;
        let (server, ada, crew) = crew(&fixture).await;
        command(&ada, &crew, invite("bo@example.com", "member")).await;
        letters(&fixture);
        let bo = fixture
            .sign_in_client(server.address, "bo@example.com")
            .await;

        let hidden = answer(&bo, &crew, "hide").await;
        let (_, listed) = ask(bo.address, "GET /api/workspaces", &bo.proof(), "").await;
        let again = answer(&bo, &crew, "hide").await;
        let newer = answer(&bo, &crew, "snooze").await;
        let elsewhere = answer(&bo, "w1", "accept").await;
        let bad_address = command(&ada, &crew, invite("not an address", "member")).await;
        let newer_action = command(&ada, &crew, json!({ "kind": "transfer" })).await;
        let nobody = command(&ada, &crew, json!({ "kind": "remove", "member": "m9" })).await;

        assert_eq!(hidden.0, "http/1.1 204 no content");
        assert_eq!(listed["invitations"], json!([]));
        assert_eq!(
            members(&ada, &crew).await["invitations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(again.0, "http/1.1 204 no content");
        assert_eq!(newer.0, "http/1.1 422 unprocessable entity");
        assert_eq!(elsewhere.0, "http/1.1 404 not found");
        assert_eq!(bad_address.1["code"], "email.invalid");
        assert_eq!(newer_action.1["code"], "request.invalid");
        assert_eq!(nobody.1["code"], "member.not_found");
        let revoke = |member: &Value| json!({ "kind": "revoke", "member": member });
        let invitation = &members(&ada, &crew).await["invitations"][0]["member"];
        assert_eq!(
            command(&ada, &crew, revoke(invitation)).await.0,
            "http/1.1 204 no content"
        );
        assert_eq!(
            answer(&bo, &crew, "accept").await.0,
            "http/1.1 404 not found"
        );
    }
}
