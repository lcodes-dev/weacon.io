use axum::extract::FromRequestParts;
use axum::http::header::{AUTHORIZATION, COOKIE, ORIGIN, SET_COOKIE};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName};

use crate::accounts::db::{Membership, User};
use crate::error::{ApiError, Result};
use crate::{AppState, accounts, now};

const COOKIE_NAME: &str = "weacon_session";
pub const SESSION_SECONDS: i64 = 365 * 24 * 60 * 60;

impl FromRequestParts<AppState> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        signed_in(state, &parts.headers)
            .await?
            .ok_or_else(|| ApiError::unauthorized("Sign in first"))
    }
}

pub struct ServerOwner(pub User);

impl FromRequestParts<AppState> for ServerOwner {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self> {
        let user = User::from_request_parts(parts, state).await?;
        if !user.is_owner {
            return Err(ApiError::forbidden("Only the server's owner can do that"));
        }
        Ok(Self(user))
    }
}

impl User {
    pub fn member_of(&self, team_id: &str) -> Result<&Membership> {
        let team = self.teams.iter().find(|team| team.id == team_id);
        team.ok_or_else(ApiError::not_found)
    }

    pub fn owner_of(&self, team_id: &str) -> Result<&Membership> {
        let team = self.member_of(team_id)?;
        if team.role != "owner" {
            return Err(ApiError::forbidden("Only the team's owner can do that"));
        }
        Ok(team)
    }
}

pub async fn signed_in(state: &AppState, headers: &HeaderMap) -> Result<Option<User>> {
    let Some(token) = session_token(headers) else {
        return Ok(None);
    };
    Ok(session_user(state, token).await?)
}

pub async fn session_user(state: &AppState, token: &str) -> sqlx::Result<Option<User>> {
    accounts::db::by_session(&state.pool, token, now() - SESSION_SECONDS).await
}

pub fn own_page(state: &AppState, headers: &HeaderMap) -> Result<()> {
    let origin = headers.get(ORIGIN);
    if origin.is_some_and(|origin| origin.as_bytes() == state.url.as_bytes()) {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "This is opened from Weacon's own pages",
    ))
}

pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(COOKIE)?
        .to_str()
        .ok()?
        .split("; ")
        .find_map(|cookie| cookie.strip_prefix(COOKIE_NAME)?.strip_prefix('='))
}

pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

pub fn session_cookie(state: &AppState, token: &str, max_age: i64) -> [(HeaderName, String); 1] {
    let secure = if state.url.starts_with("https://") {
        "; Secure"
    } else {
        ""
    };
    let cookie =
        format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}");
    [(SET_COOKIE, cookie)]
}

pub fn normalized(email: &str) -> String {
    email.trim().to_lowercase()
}

pub fn random_token() -> String {
    format!("{:032x}", rand::random::<u128>())
}
