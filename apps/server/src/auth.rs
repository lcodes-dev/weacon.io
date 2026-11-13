use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::StatusCode;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use weacon_protocol::{MemberId, WorkspaceId, code};

use crate::db::Workspace;
use crate::http::{Shared, failure, refused};
use crate::sign_in::token_hash;

/// Who asks for something in the workspace a route names as `{workspace}`: a member of it,
/// signed in. Anyone else is refused the same way, whether or not there is such a workspace.
pub(crate) struct Member {
    pub(crate) workspace: Workspace,
    pub(crate) id: MemberId,
    /// The hash of the session token that proved it.
    pub(crate) token_hash: [u8; 32],
}

impl FromRequestParts<Shared> for Member {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, shared: &Shared) -> Result<Self, Response> {
        let bad_request = |_| StatusCode::BAD_REQUEST.into_response();
        let params = RawPathParams::from_request_parts(parts, shared)
            .await
            .map_err(bad_request)?;
        let workspace = params
            .iter()
            .find(|(name, _)| *name == "workspace")
            .and_then(|(_, id)| WorkspaceId::try_from(id.to_owned()).ok())
            .ok_or_else(|| StatusCode::BAD_REQUEST.into_response())?;
        let required = || refused(code!("sign_in.required"));
        let token_hash = parts
            .headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .map(token_hash)
            .ok_or_else(required)?;
        let found = shared
            .db
            .member_workspace(&workspace, &token_hash, shared.now())
            .await
            .map_err(|fault| failure(shared, fault))?;
        let (workspace, id) = found.ok_or_else(required)?;
        Ok(Self {
            workspace,
            id,
            token_hash,
        })
    }
}
