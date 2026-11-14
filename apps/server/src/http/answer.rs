use std::marker::PhantomData;
use std::sync::Arc;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use weacon_observability::{Fault, Field};
use weacon_protocol::{Code, ErrorBody, code};

use crate::shared::Shared;

/// Answers that the client must sign in, or try again, with `code`.
pub(crate) fn unauthorized(code: Code) -> Response {
    error(StatusCode::UNAUTHORIZED, code, false)
}

/// Answers that the request's body isn't one the server can take, with `code`.
pub(crate) fn invalid(code: Code) -> Response {
    error(StatusCode::UNPROCESSABLE_ENTITY, code, false)
}

/// Answers that there is no such thing, or none the asker may see, with `code`.
pub(crate) fn not_found(code: Code) -> Response {
    error(StatusCode::NOT_FOUND, code, false)
}

/// Answers that the request isn't from the server's own pages.
pub(crate) fn cross_site() -> Response {
    error(StatusCode::FORBIDDEN, code!("request.cross_site"), false)
}

/// Answers that this client has asked too often for now.
pub(crate) fn too_many(code: Code) -> Response {
    error(StatusCode::TOO_MANY_REQUESTS, code, true)
}

/// Answers that the server failed with `fault`, which the request's log record logs and
/// reports, with its id.
pub(crate) fn failure(fault: Fault) -> Response {
    let mut response = error(
        StatusCode::INTERNAL_SERVER_ERROR,
        fault.code().clone(),
        true,
    );
    response.extensions_mut().insert(Failed(Arc::new(fault)));
    response
}

/// Answers `fault` with its status and retryable flag in `refusals`, or as the server failing
/// when it isn't there.
fn refuse(refusals: &[(Code, StatusCode, bool)], fault: Fault) -> Response {
    match refusals.iter().find(|(code, ..)| code == fault.code()) {
        Some((code, status, retryable)) => error(*status, code.clone(), *retryable),
        None => failure(fault),
    }
}

/// A feature's refusals: each code it answers, with its status and whether trying again may
/// work; anything else is the server failing.
pub(crate) trait Refusals {
    const REFUSALS: &'static [(Code, StatusCode, bool)];
}

/// Why a request of `R`'s feature wasn't done: answered from `R`'s refusals.
pub(crate) struct Refused<R>(Fault, PhantomData<R>);

impl<R> From<Fault> for Refused<R> {
    fn from(fault: Fault) -> Self {
        Self(fault, PhantomData)
    }
}

impl<R: Refusals> IntoResponse for Refused<R> {
    fn into_response(self) -> Response {
        refuse(R::REFUSALS, self.0)
    }
}

/// The fault a response answers, until `log_request` takes it.
#[derive(Clone)]
pub(super) struct Failed(pub(super) Arc<Fault>);

pub(crate) fn error(status: StatusCode, code: Code, retryable: bool) -> Response {
    (status, Json(ErrorBody { code, retryable })).into_response()
}

/// Logs `fault` as `event`, with `fields`, and reports it.
pub(crate) fn report(
    shared: &Shared,
    event: &'static str,
    fault: Arc<Fault>,
    fields: &[(&'static str, Field)],
) {
    let err = [("err", Field::error(&*fault))];
    shared.log.error(event, &[fields, &err].concat());
    let (reporter, log) = (Arc::clone(&shared.reporter), shared.log.clone());
    // Sending a report blocks, so it runs beside the request rather than holding it up.
    drop(tokio::task::spawn_blocking(move || {
        reporter.capture_error(&fault, fault.code(), &log);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_refusal_in_the_table_gets_its_status_and_flag_and_any_other_is_a_failure() {
        let refusals = [
            (code!("doc.stale"), StatusCode::CONFLICT, false),
            (code!("db.exists"), StatusCode::CONFLICT, true),
        ];
        let fault = |code| Fault::new(code, "Made up");
        let body = |response: Response| async move {
            let body = axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap();
            serde_json::from_slice::<serde_json::Value>(&body).unwrap()
        };

        let stale = refuse(&refusals, fault(code!("doc.stale")));
        let taken = refuse(&refusals, fault(code!("db.exists")));
        let broken = refuse(&refusals, fault(code!("db.failed")));

        assert_eq!(
            (stale.status(), taken.status()),
            (StatusCode::CONFLICT, StatusCode::CONFLICT)
        );
        assert!(stale.extensions().get::<Failed>().is_none());
        assert_eq!(
            [body(stale).await, body(taken).await],
            [
                serde_json::json!({ "code": "doc.stale", "retryable": false }),
                serde_json::json!({ "code": "db.exists", "retryable": true }),
            ]
        );
        assert_eq!(broken.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(broken.extensions().get::<Failed>().is_some());
    }
}
