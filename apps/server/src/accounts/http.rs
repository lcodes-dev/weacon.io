use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use serde::Deserialize;
use serde_json::{Value, json};

use super::db;
use super::db::{Invitation, Membership, User};
use crate::auth::{
    SESSION_SECONDS, normalized, random_token, session_cookie, session_token, session_user, signed_in,
};
use crate::error::{ApiError, Result};
use crate::live::Topic;
use crate::settings::mail::Mail;
use crate::{AppState, live, now, settings};

const CODE_SECONDS: i64 = 10 * 60;
const RENEW_SECONDS: i64 = 24 * 60 * 60;

pub async fn show_session(State(state): State<AppState>, headers: HeaderMap) -> Result<impl IntoResponse> {
    let setup_needed = !db::any_exist(&state.pool).await?;
    let user = signed_in(&state, &headers).await?;
    let mut cookie = None;
    if let (Some(_), Some(token)) = (&user, session_token(&headers))
        && db::renew_session(&state.pool, token, now(), now() - RENEW_SECONDS).await?
    {
        cookie = Some(session_cookie(&state, token, SESSION_SECONDS));
    }
    let session = json!({"setup_needed": setup_needed, "user": user});
    Ok((cookie, Json(session)))
}

#[derive(Deserialize)]
pub struct CodeRequest {
    email: String,
    name: Option<String>,
}

pub async fn send_code(
    State(state): State<AppState>,
    Json(request): Json<CodeRequest>,
) -> Result<StatusCode> {
    let email = normalized(&request.email);
    if !settings::mail::is_address(&email) {
        return Err(ApiError::invalid("That isn't an email address"));
    }
    let name = request.name.as_deref().map(str::trim).filter(|name| !name.is_empty());
    if name.is_none() && !db::exists(&state.pool, &email).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let code = format!("{:06}", rand::random_range(0..1_000_000));
    if !db::save_code(&state.pool, &email, &code, name, now(), CODE_SECONDS).await? {
        return Ok(StatusCode::NO_CONTENT);
    }
    let mail = Mail {
        subject: "Your Weacon sign-in code",
        text: "Enter this code to sign in. It works for 10 minutes.",
        code: Some(&code),
        button: None,
    };
    if let Err(error) = settings::mail::send(&state, &email, &mail).await {
        tracing::error!("mail to {email} failed: {error}");
        db::end_code_wait(&state.pool, &email).await?;
        return Err(ApiError::bad_gateway("The mail could not be sent"));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SignIn {
    email: String,
    code: String,
}

pub async fn create_session(
    State(state): State<AppState>,
    Json(sign_in): Json<SignIn>,
) -> Result<impl IntoResponse> {
    let email = normalized(&sign_in.email);
    let token = random_token();
    if !db::sign_in(&state.pool, &email, sign_in.code.trim(), &token, now()).await? {
        let message = "That code is wrong, has expired or was tried too often";
        return Err(ApiError::unauthorized(message));
    }
    let user = session_user(&state, &token).await?;
    Ok((session_cookie(&state, &token, SESSION_SECONDS), Json(user)))
}

pub async fn delete_session(State(state): State<AppState>, headers: HeaderMap) -> Result<impl IntoResponse> {
    if let Some(token) = session_token(&headers) {
        db::sign_out(&state.pool, token).await?;
    }
    Ok((session_cookie(&state, "", 0), StatusCode::NO_CONTENT))
}

pub async fn show_team(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
) -> Result<Json<Value>> {
    let team = user.member_of(&team_id)?;
    let members = db::members(&state.pool, &team_id).await?;
    let invites = db::invites(&state.pool, &team_id).await?;
    let writer = settings::db::writer(&state.pool, &state.server_key, &team_id).await?;
    let api_key = settings::db::api_key(&state.pool, &state.server_key, &team_id).await?;
    Ok(Json(json!({
        "name": team.name,
        "members": members,
        "invites": invites,
        "writer": writer.map(|writer| writer.public()),
        "has_key": api_key.is_some(),
    })))
}

#[derive(Deserialize)]
pub struct Rename {
    name: String,
}

pub async fn update_team(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(rename): Json<Rename>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    let name = rename.name.trim();
    if name.is_empty() {
        return Err(ApiError::invalid("A team needs a name"));
    }
    db::rename_team(&state.pool, &team_id, name).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_member(
    State(state): State<AppState>,
    user: User,
    Path((team_id, user_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    if user_id != user.id {
        user.owner_of(&team_id)?;
    }
    if !db::remove_member(&state.pool, &team_id, &user_id).await? {
        return Err(ApiError::invalid("The team's owner can't leave"));
    }
    live::team(&state, &team_id, Topic::Groups).await;
    Ok(StatusCode::NO_CONTENT)
}

const INVITE_SECONDS: i64 = 7 * 24 * 60 * 60;

#[derive(Deserialize)]
pub struct NewInvites {
    emails: Vec<String>,
}

pub async fn create_invites(
    State(state): State<AppState>,
    user: User,
    Path(team_id): Path<String>,
    Json(invites): Json<NewInvites>,
) -> Result<StatusCode> {
    let team = user.owner_of(&team_id)?;
    let mut emails: Vec<String> = invites.emails.iter().map(|email| normalized(email)).collect();
    emails.sort();
    emails.dedup();
    for email in &emails {
        if !settings::mail::is_address(email) {
            return Err(ApiError::invalid(format!("{email} isn't an email address")));
        }
        if db::has_member_with_email(&state.pool, &team_id, email).await? {
            return Err(ApiError::invalid(format!("{email} is already in the team")));
        }
    }
    for email in &emails {
        send_invite(&state, &user, team, email).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn send_invite(state: &AppState, user: &User, team: &Membership, email: &str) -> Result<()> {
    let token = random_token();
    let expires_at = now() + INVITE_SECONDS;
    db::save_invite(&state.pool, &team.id, email, &token, &user.id, expires_at).await?;

    let subject = format!("Join {} on Weacon", team.name);
    let text = format!(
        "{} invited you to {} on Weacon.\n\nSign in or create an account with this email \
         address to join. The invite works for 7 days.",
        user.name, team.name
    );
    let link = format!("{}/invite/{token}", state.url);
    let mail = Mail { subject: &subject, text: &text, code: None, button: Some(("Join the team", &link)) };
    settings::mail::send(state, email, &mail).await.map_err(|error| {
        tracing::error!("mail to {email} failed: {error}");
        ApiError::bad_gateway(format!("The mail to {email} could not be sent"))
    })
}

pub async fn delete_invite(
    State(state): State<AppState>,
    user: User,
    Path((team_id, invite_id)): Path<(String, String)>,
) -> Result<StatusCode> {
    user.owner_of(&team_id)?;
    db::delete_invite(&state.pool, &team_id, &invite_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn show_invite(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> Result<Json<Invitation>> {
    Ok(Json(invitation(&state, &token).await?))
}

pub async fn accept_invite(
    State(state): State<AppState>,
    user: User,
    Path(token): Path<String>,
) -> Result<Json<Value>> {
    let invitation = invitation(&state, &token).await?;
    if invitation.email != user.email {
        let message = format!("This invite is for {}", invitation.email);
        return Err(ApiError::forbidden(message));
    }
    let team_id = db::accept_invite(&state.pool, &token, &user.id).await?.ok_or_else(gone)?;
    Ok(Json(json!({"team_id": team_id})))
}

async fn invitation(state: &AppState, token: &str) -> Result<Invitation> {
    let invitation = db::invitation(&state.pool, token, now()).await?;
    invitation.ok_or_else(gone)
}

fn gone() -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "This invite no longer works")
}
