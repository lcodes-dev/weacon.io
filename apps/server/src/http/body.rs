use axum::Json;
use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use weacon_observability::Fault;
use weacon_protocol::code;

use super::error;

pub(super) const MAX_BODY: usize = 16 * 1024;

/// A JSON request body; one the server can't read is refused with an `ErrorBody`, as every
/// other client error is.
pub(crate) struct Body<T>(pub(crate) T);

/// Why a request's body couldn't be read.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Unreadable {
    TooLarge,
    NotJson,
    Invalid,
}

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Body<T> {
    type Rejection = Unreadable;

    async fn from_request(request: Request, state: &S) -> Result<Self, Unreadable> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(body)| Self(body))
            .map_err(|rejection| match rejection.status() {
                StatusCode::PAYLOAD_TOO_LARGE => Unreadable::TooLarge,
                StatusCode::UNSUPPORTED_MEDIA_TYPE => Unreadable::NotJson,
                _ => Unreadable::Invalid,
            })
    }
}

impl IntoResponse for Unreadable {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, code!("request.too_large")),
            Self::NotJson => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                code!("request.not_json"),
            ),
            Self::Invalid => (StatusCode::BAD_REQUEST, code!("request.invalid")),
        };
        error(status, code, false)
    }
}

/// A request's body, or the fault `too_large` makes past the size limit and `invalid` makes
/// otherwise.
pub(crate) fn parsed<T>(
    body: Result<Body<T>, Unreadable>,
    too_large: impl FnOnce() -> Fault,
    invalid: impl FnOnce() -> Fault,
) -> Result<T, Fault> {
    match body {
        Ok(Body(body)) => Ok(body),
        Err(Unreadable::TooLarge) => Err(too_large()),
        Err(_) => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::header;

    use super::*;
    use crate::test_support::{Fixture, request};

    // How a body of `content_type` is refused.
    async fn refusal(content_type: &str, body: &str) -> (StatusCode, serde_json::Value) {
        let request = Request::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        let answer = Body::<serde_json::Value>::from_request(request, &())
            .await
            .err()
            .unwrap()
            .into_response();
        let status = answer.status();
        let bytes = axum::body::to_bytes(answer.into_body(), 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn a_body_that_cant_be_read_is_answered_with_an_error_body() {
        let code = |code: &str| serde_json::json!({ "code": code, "retryable": false });

        assert_eq!(
            refusal("application/json", "{").await,
            (StatusCode::BAD_REQUEST, code("request.invalid"))
        );
        assert_eq!(
            refusal("text/plain", "{}").await,
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, code("request.not_json"))
        );
        let fixture = Fixture::new().await;
        let running = fixture.run().await;
        let big = format!(r#"{{"email":"{}"}}"#, "a".repeat(MAX_BODY));
        let (head, _) = request(running.address, "POST /api/sign-in", &big).await;
        assert_eq!(head[0], "http/1.1 413 payload too large");
    }

    #[test]
    fn a_body_too_large_and_one_otherwise_unread_get_their_own_faults() {
        let too_large = || Fault::new(code!("doc.too_large"), "Made up");
        let invalid = || Fault::new(code!("doc.invalid"), "Made up");
        let code = |body| {
            parsed::<()>(body, too_large, invalid)
                .unwrap_err()
                .code()
                .clone()
        };

        assert_eq!(parsed(Ok(Body(1)), too_large, invalid).unwrap(), 1);
        assert_eq!(code(Err(Unreadable::TooLarge)), code!("doc.too_large"));
        assert_eq!(code(Err(Unreadable::NotJson)), code!("doc.invalid"));
        assert_eq!(code(Err(Unreadable::Invalid)), code!("doc.invalid"));
    }
}
