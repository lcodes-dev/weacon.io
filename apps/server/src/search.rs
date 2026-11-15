//! Full-text search over what a member can read in their workspace.

pub(crate) mod query;

use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use weacon_observability::Fault;
use weacon_protocol::{Code, SearchAnswer, SearchQuery, code};

use crate::auth::Member;
use crate::http::{self, Body, Refusals, Unreadable, parsed};
use crate::shared::Shared;

/// What the server offers to clients that search a workspace.
pub(crate) const SEARCH: Code = code!("workspace.search");

const LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 50;

pub(crate) fn routes() -> Router<Shared> {
    Router::new().route("/api/workspaces/{workspace}/search", post(search))
}

/// `POST /api/workspaces/{workspace}/search`: what in the workspace matches the query's text.
/// Every member reads every task and doc, so membership is the only check.
async fn search(
    member: Member,
    body: Result<Body<SearchQuery>, Unreadable>,
) -> Result<Json<SearchAnswer>, Refused> {
    let asked = parsed(body, too_large, invalid)?;
    let limit = match asked.limit {
        None => LIMIT,
        Some(limit @ 1..=MAX_LIMIT) => limit,
        Some(_) => return Err(invalid().into()),
    };
    let Some(query) = query::parse(&asked.text).map_err(|_| too_long())? else {
        return Ok(Json(SearchAnswer { hits: Vec::new() }));
    };
    let hits = member.workspace.search(&query, limit).await?;
    Ok(Json(SearchAnswer { hits }))
}

fn invalid() -> Fault {
    Fault::new(code!("search.invalid"), "The search request isn't valid")
}

fn too_long() -> Fault {
    Fault::new(code!("search.too_long"), "The search text is too long")
}

fn too_large() -> Fault {
    Fault::new(code!("search.too_large"), "The search request is too large")
}

type Refused = http::Refused<Search>;

/// The search feature's refusals.
pub(crate) struct Search;

impl Refusals for Search {
    const REFUSALS: &'static [(Code, StatusCode, bool)] = &[
        (code!("search.invalid"), StatusCode::BAD_REQUEST, false),
        (code!("search.too_long"), StatusCode::BAD_REQUEST, false),
        (
            code!("search.too_large"),
            StatusCode::PAYLOAD_TOO_LARGE,
            false,
        ),
    ];
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{SearchHit, TaskCreate};

    use super::*;
    use crate::test_support::{Client, Fixture, Running, error_code, request_with, status};

    struct Ada(Client);

    impl Ada {
        async fn send(&self, head: &str, body: &str) -> (String, String) {
            self.0.send(head, body).await
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
        let (server, client) = fixture.client("ada@example.com").await;
        (server, Ada(client))
    }

    fn titles(hits: &[SearchHit]) -> Vec<&str> {
        hits.iter()
            .map(|hit| match hit {
                SearchHit::Task { title, .. }
                | SearchHit::Doc { title, .. }
                | SearchHit::Chat { title, .. } => title.as_str(),
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
            assert_eq!(error_code(&answer), code);
        }
        let (refused, answer) = ada.search(&huge).await;
        assert_eq!(refused, "http/1.1 413 payload too large");
        assert_eq!(error_code(&answer), "search.too_large");
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
