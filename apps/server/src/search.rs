//! Full-text search over what a member can read in their workspace.

pub(crate) mod query;

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use weacon_observability::Fault;
use weacon_protocol::{Code, SearchAnswer, SearchHit, SearchQuery, code};

use crate::auth::Member;
use crate::http::{Body, Unreadable, error, failure};

/// What the server offers to clients that search a workspace.
pub(crate) const SEARCH: Code = code!("workspace.search");

const LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 50;

/// `POST /api/workspaces/{workspace}/search`: what in the workspace matches the query's text.
/// Every member reads every task and doc, so membership is the only check.
pub(crate) async fn search(
    member: Member,
    body: Result<Body<SearchQuery>, Unreadable>,
) -> Response {
    match found(&member, body).await {
        Ok(hits) => Json(SearchAnswer { hits }).into_response(),
        Err(fault) => answer(fault),
    }
}

async fn found(
    member: &Member,
    body: Result<Body<SearchQuery>, Unreadable>,
) -> Result<Vec<SearchHit>, Fault> {
    let Body(asked) = body.map_err(|unreadable| match unreadable {
        Unreadable::TooLarge => {
            Fault::new(code!("search.too_large"), "The search request is too large")
        }
        Unreadable::NotJson | Unreadable::Invalid => invalid(),
    })?;
    let limit = match asked.limit {
        None => LIMIT,
        Some(limit @ 1..=MAX_LIMIT) => limit,
        Some(_) => return Err(invalid()),
    };
    let Some(query) = query::parse(&asked.text).map_err(|_| too_long())? else {
        return Ok(Vec::new());
    };
    member.workspace.search(&query, limit).await
}

fn invalid() -> Fault {
    Fault::new(code!("search.invalid"), "The search request isn't valid")
}

fn too_long() -> Fault {
    Fault::new(code!("search.too_long"), "The search text is too long")
}

const REFUSALS: [(Code, StatusCode); 3] = [
    (code!("search.invalid"), StatusCode::BAD_REQUEST),
    (code!("search.too_long"), StatusCode::BAD_REQUEST),
    (code!("search.too_large"), StatusCode::PAYLOAD_TOO_LARGE),
];

/// Answers a refusal with its status, and anything else as the server failing.
fn answer(fault: Fault) -> Response {
    let code = fault.code().clone();
    match REFUSALS.iter().find(|(refusal, _)| *refusal == code) {
        Some((_, status)) => error(*status, code, false),
        None => failure(fault),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use weacon_protocol::TaskCreate;

    use super::*;
    use crate::test_support::{Fixture, Running, bearer, refusal, request_with, status};

    struct Ada {
        address: SocketAddr,
        auth: String,
    }

    impl Ada {
        async fn send(&self, head: &str, body: &str) -> (String, String) {
            let (head, body) = request_with(self.address, head, &self.auth, body).await;
            (status(&head).to_owned(), body)
        }

        async fn task(&self, title: &str) {
            let create = TaskCreate {
                title: title.to_owned(),
                before: None,
            };
            let body = serde_json::to_string(&create).unwrap();
            let (status, _) = self.send("POST /api/workspaces/w1/tasks", &body).await;
            assert_eq!(status, "http/1.1 201 created");
        }

        async fn search(&self, body: &str) -> (String, String) {
            self.send("POST /api/workspaces/w1/search", body).await
        }

        async fn hits(&self, text: &str) -> Vec<SearchHit> {
            let asked = SearchQuery {
                text: text.to_owned(),
                limit: None,
            };
            let (status, body) = self.search(&serde_json::to_string(&asked).unwrap()).await;
            assert_eq!(status, "http/1.1 200 ok", "{body}");
            serde_json::from_str::<SearchAnswer>(&body).unwrap().hits
        }
    }

    async fn ada(fixture: &Fixture) -> (Running, Ada) {
        let (server, body) = fixture.signed_in("ada@example.com").await;
        let ada = Ada {
            address: server.address,
            auth: bearer(&body),
        };
        (server, ada)
    }

    fn titles(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter()
            .map(|hit| match hit {
                SearchHit::Task { title, .. } | SearchHit::Doc { title, .. } => title.as_str(),
                SearchHit::Newer => "",
            })
            .collect()
    }

    #[tokio::test]
    async fn a_member_finds_their_workspaces_tasks() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        ada.task("Write the release notes").await;
        ada.task("Plan the launch").await;

        assert_eq!(
            titles(&ada.hits("release").await),
            ["Write the release notes"]
        );
        assert_eq!(titles(&ada.hits("the").await).len(), 2);
        assert!(ada.hits("to").await.is_empty());
        assert_eq!(
            titles(&ada.hits("rel\0ease").await),
            ["Write the release notes"]
        );
        let hits = ada.hits("launch").await;
        let SearchHit::Task { snippet, .. } = &hits[0] else {
            panic!("{hits:?}");
        };
        assert_eq!(snippet.text, "Plan the launch");
        assert_eq!(snippet.marks, [weacon_protocol::Mark { start: 9, end: 15 }]);
    }

    #[tokio::test]
    async fn a_search_is_bounded_and_needs_a_member() {
        let fixture = Fixture::new().await;
        let (server, ada) = ada(&fixture).await;
        for title in ["Release one", "Release two", "Release three"] {
            ada.task(title).await;
        }
        let asked = |text: &str, limit: Option<u32>| {
            serde_json::to_string(&SearchQuery {
                text: text.to_owned(),
                limit,
            })
            .unwrap()
        };

        let (answered, body) = ada.search(&asked("release", Some(2))).await;
        assert_eq!(answered, "http/1.1 200 ok");
        assert_eq!(
            serde_json::from_str::<SearchAnswer>(&body)
                .unwrap()
                .hits
                .len(),
            2
        );
        let long = "a".repeat(query::MAX_TEXT + 1);
        let huge = format!("{{\"text\":\"{}\"}}", "a".repeat(32 * 1024));
        for (body, code) in [
            (asked("release", Some(0)), "search.invalid"),
            (asked("release", Some(MAX_LIMIT + 1)), "search.invalid"),
            ("{\"limit\":1}".to_owned(), "search.invalid"),
            (asked(&long, None), "search.too_long"),
        ] {
            let (refused, answer) = ada.search(&body).await;
            assert_eq!(refused, "http/1.1 400 bad request", "{code}");
            assert_eq!(refusal(&answer), code);
        }
        let (refused, answer) = ada.search(&huge).await;
        assert_eq!(refused, "http/1.1 413 payload too large");
        assert_eq!(refusal(&answer), "search.too_large");
        assert!(
            ada.search(&asked("release", Some(MAX_LIMIT)))
                .await
                .0
                .ends_with("200 ok")
        );
        let (head, _) = request_with(
            server.address,
            "POST /api/workspaces/w1/search",
            "",
            &asked("release", None),
        )
        .await;
        assert_eq!(status(&head), "http/1.1 401 unauthorized");
    }

    #[tokio::test]
    async fn what_is_searched_and_found_reaches_no_log() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        ada.task("MARKER-TITLE").await;

        assert_eq!(titles(&ada.hits("MARKER-TITLE").await), ["MARKER-TITLE"]);
        let long = format!("MARKER-TEXT {}", "a".repeat(query::MAX_TEXT));
        let (status, _) = ada.search(&format!("{{\"text\":\"{long}\"}}")).await;
        assert_eq!(status, "http/1.1 400 bad request");
        let records = fixture.records.lock().unwrap().clone();
        assert!(records.iter().any(|record| {
            record
                .fields
                .get("route")
                .is_some_and(|route| route == "/api/workspaces/{workspace}/search")
        }));
        assert!(!serde_json::to_string(&records).unwrap().contains("MARKER"));
    }
}
