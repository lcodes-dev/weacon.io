use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{normalized, random_token};
use crate::db::teams::Invitation;
use crate::db::users::{Membership, User};
use crate::error::{ApiError, Result};
use crate::mail::{self, Mail};
use crate::{AppState, db, now};

const INVITE_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Deserialize)]
pub struct NewInvites {
    emails: Vec<String>,
}

pub async fn create(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(invites): Json<NewInvites>,
) -> Result<StatusCode> {
    let team = user.owner_of(&team_id)?;
    let mut emails: Vec<String> = invites
        .emails
        .iter()
        .map(|email| normalized(email))
        .collect();
    emails.sort();
    emails.dedup();
    // Every address is checked before any mail goes out.
    for email in &emails {
        if !mail::is_address(email) {
            return Err(ApiError::invalid(format!("{email} isn't an email address")));
        }
        if db::teams::has_member_with_email(&state.pool, &team_id, email).await? {
            return Err(ApiError::invalid(format!("{email} is already in the team")));
        }
    }
    for email in &emails {
        send(&state, &user, team, email).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn send(state: &AppState, user: &User, team: &Membership, email: &str) -> Result<()> {
    let token = random_token();
    let expires_at = now() + INVITE_SECONDS;
    db::teams::save_invite(&state.pool, &team.id, email, &token, &user.id, expires_at).await?;

    let subject = format!("Join {} on Weacon", team.name);
    let text = format!(
        "{} invited you to {} on Weacon.\n\nSign in or create an account with this email \
         address to join. The invite works for 7 days.",
        user.name, team.name
    );
    let link = format!("{}/invite/{token}", state.url);
    let mail = Mail {
        subject: &subject,
        text: &text,
        code: None,
        button: Some(("Join the team", &link)),
    };
    mail::send(state, email, &mail).await.map_err(|error| {
        tracing::error!("mail to {email} failed: {error}");
        ApiError::bad_gateway(format!("The mail to {email} could not be sent"))
    })
}

pub async fn delete(
    State(state): State<AppState>,
    user: User,
    Path((team_id, invite_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    db::teams::delete_invite(&state.pool, &team_id, &invite_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// What the person opening an invite link sees, signed in or not.
pub async fn show(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Invitation>> {
    Ok(Json(invitation(&state, &token).await?))
}

pub async fn accept(
    State(state): State<AppState>,
    user: User,
    Path(token): Path<String>,
) -> Result<Json<Value>> {
    let invitation = invitation(&state, &token).await?;
    if invitation.email != user.email {
        let message = format!("This invite is for {}", invitation.email);
        return Err(ApiError::forbidden(message));
    }
    // Revoked or used since the check above.
    let team_id = db::teams::accept_invite(&state.pool, &token, &user.id).await?;
    let team_id = team_id.ok_or_else(gone)?;
    Ok(Json(json!({"team_id": team_id})))
}

async fn invitation(state: &AppState, token: &str) -> Result<Invitation> {
    let invitation = db::teams::invitation(&state.pool, token, now()).await?;
    invitation.ok_or_else(gone)
}

fn gone() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "This invite no longer works")
}
