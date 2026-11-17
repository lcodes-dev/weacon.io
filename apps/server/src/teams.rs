use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::normalized;
use crate::db::{self, Invitation, Team, User};
use crate::mail::Mail;
use crate::{ApiError, AppState, now};

const INVITE_SECONDS: i64 = 7 * 24 * 60 * 60;

pub fn refused(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::UNPROCESSABLE_ENTITY, message.into())
}

/// A team the user isn't in looks like one that doesn't exist.
pub fn membership(user: &User, team_id: i64) -> Result<&Team, ApiError> {
    let team = user.teams.iter().find(|team| team.id == team_id);
    team.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "not found".into()))
}

pub fn owned(user: &User, team_id: i64) -> Result<&Team, ApiError> {
    let team = membership(user, team_id)?;
    if team.role == "owner" {
        return Ok(team);
    }
    let message = "Only the team's owner can do that";
    Err(ApiError(StatusCode::FORBIDDEN, message.into()))
}

pub async fn team(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
) -> Result<Json<Value>, ApiError> {
    let team = membership(&user, team_id)?;
    let members = db::members(&state.pool, team_id).await?;
    let invites = db::invites(&state.pool, team_id).await?;
    Ok(Json(
        json!({"name": team.name, "members": members, "invites": invites}),
    ))
}

#[derive(Deserialize)]
pub struct Rename {
    name: String,
}

pub async fn rename(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
    Json(rename): Json<Rename>,
) -> Result<StatusCode, ApiError> {
    owned(&user, team_id)?;
    let name = rename.name.trim();
    if name.is_empty() {
        return Err(refused("A team needs a name"));
    }
    db::rename_team(&state.pool, team_id, name).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct NewInvites {
    emails: Vec<String>,
}

pub async fn invite(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<i64>,
    Json(invites): Json<NewInvites>,
) -> Result<StatusCode, ApiError> {
    let team = owned(&user, team_id)?;
    let emails: Vec<String> = invites
        .emails
        .iter()
        .map(|email| normalized(email))
        .collect();
    // Every address is checked before any mail goes out.
    for email in &emails {
        if !email.contains('@') {
            return Err(refused(format!("{email} isn't an email address")));
        }
        if db::is_member(&state.pool, team_id, email).await? {
            return Err(refused(format!("{email} is already in the team")));
        }
    }
    for email in &emails {
        send_invite(&state, &user, team, email).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn send_invite(
    state: &AppState,
    user: &User,
    team: &Team,
    email: &str,
) -> Result<(), ApiError> {
    let token = format!("{:032x}", rand::random::<u128>());
    let expires_at = now() + INVITE_SECONDS;
    db::save_invite(&state.pool, team.id, email, &token, user.id, expires_at).await?;

    let subject = format!("Join {} on Weacon", team.name);
    let text = format!(
        "{} invited you to {} on Weacon.\n\nSign in or create an account with this email address to join. The invite works for 7 days.",
        user.name, team.name
    );
    let link = format!("{}/invite/{token}", state.url);
    let mail = Mail {
        subject: &subject,
        text: &text,
        code: None,
        button: Some(("Join the team", &link)),
    };
    state.mailer.send(email, &mail).await.map_err(|error| {
        tracing::error!("mail to {email} failed: {error}");
        let message = format!("The mail to {email} could not be sent");
        ApiError(StatusCode::BAD_GATEWAY, message)
    })
}

pub async fn revoke(
    State(state): State<AppState>,
    user: User,
    Path((team_id, invite_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    owned(&user, team_id)?;
    db::delete_invite(&state.pool, team_id, invite_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The owner removes a member, or a member leaves.
pub async fn remove_member(
    State(state): State<AppState>,
    user: User,
    Path((team_id, user_id)): Path<(i64, i64)>,
) -> Result<StatusCode, ApiError> {
    if user_id == user.id {
        membership(&user, team_id)?;
    } else {
        owned(&user, team_id)?;
    }
    if !db::remove_member(&state.pool, team_id, user_id).await? {
        return Err(refused("The team's owner can't leave"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn invited(state: &AppState, token: &str) -> Result<Invitation, ApiError> {
    let invitation = db::invitation(&state.pool, token, now()).await?;
    invitation.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "This invite no longer works".into()))
}

pub async fn invitation(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Invitation>, ApiError> {
    Ok(Json(invited(&state, &token).await?))
}

pub async fn accept(
    State(state): State<AppState>,
    user: User,
    Path(token): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let invitation = invited(&state, &token).await?;
    if invitation.email != user.email {
        let message = format!("This invite is for {}", invitation.email);
        return Err(ApiError(StatusCode::FORBIDDEN, message));
    }
    let team_id = db::accept_invite(&state.pool, &token, user.id).await?;
    Ok(Json(json!({"team_id": team_id})))
}
