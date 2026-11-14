use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use weacon_model::{Attribution, Doc, DocText, DocTextError, Revision};
use weacon_observability::Fault;
use weacon_protocol::{
    Code, DocContent, DocHistory, DocId, DocList, DocRecord, DocRevise, WorkspaceId, code,
};

use crate::app::new_id;
use crate::auth::Member;
use crate::http::{self, Body, Refusals, Shared, Unreadable, parsed};

/// The most a doc request's body may be: room for the largest doc the model allows, even
/// when JSON escapes each byte of its Markdown as `\u00XX`, with its pins.
pub(crate) const MAX_BODY: usize = 2 * 1024 * 1024;
/// How many revisions a history holds, newest first; older ones are still read by version.
const HISTORY: u32 = 1000;

/// `GET /api/workspaces/{workspace}/docs`: the workspace's docs, by title, without bodies.
pub(crate) async fn list(member: Member) -> Result<Json<DocList>, Refused> {
    let docs = member.workspace.docs().await?;
    Ok(Json(DocList { docs }))
}

/// `POST /api/workspaces/{workspace}/docs`: makes a doc at its first version.
pub(crate) async fn create(
    State(shared): State<Shared>,
    member: Member,
    body: Result<Body<DocContent>, Unreadable>,
) -> Result<impl IntoResponse, Refused> {
    let text = text(parsed(body, too_large, invalid)?)?;
    let id = DocId::try_from(new_id()).map_err(|_| invalid())?;
    let doc = Doc::new(id, text);
    let attribution = Attribution::by(member.id.clone(), shared.now());
    member
        .workspace
        .add_doc(&doc, &attribution)
        .await
        .inspect_err(|fault| warn_if_full(&shared, &member, fault))?;
    shared.changed(member.workspace.id());
    Ok((StatusCode::CREATED, Json(record(doc))))
}

/// `GET /api/workspaces/{workspace}/docs/{doc}`: the doc as it is now.
pub(crate) async fn read(
    member: Member,
    path: Result<Path<(WorkspaceId, DocId)>, PathRejection>,
) -> Result<Json<DocRecord>, Refused> {
    let Path((_, id)) = path.map_err(|_| invalid())?;
    Ok(Json(record(member.workspace.doc(&id, None).await?)))
}

/// `POST /api/workspaces/{workspace}/docs/{doc}`: replaces the text of a doc the client read
/// at `version`.
pub(crate) async fn revise(
    State(shared): State<Shared>,
    member: Member,
    path: Result<Path<(WorkspaceId, DocId)>, PathRejection>,
    body: Result<Body<DocRevise>, Unreadable>,
) -> Result<Json<DocRecord>, Refused> {
    let Path((_, id)) = path.map_err(|_| invalid())?;
    let body = parsed(body, too_large, invalid)?;
    let text = text(body.content)?;
    let attribution = Attribution::by(member.id.clone(), shared.now());
    let read_at = Revision::new(body.version);
    let doc = member
        .workspace
        .revise_doc(&id, read_at, text, &attribution)
        .await
        .inspect_err(|fault| warn_if_full(&shared, &member, fault))?;
    shared.changed(member.workspace.id());
    Ok(Json(record(doc)))
}

/// `GET /api/workspaces/{workspace}/docs/{doc}/revisions`: who made the doc's newest
/// revisions, and when.
pub(crate) async fn history(
    member: Member,
    path: Result<Path<(WorkspaceId, DocId)>, PathRejection>,
) -> Result<Json<DocHistory>, Refused> {
    let Path((_, id)) = path.map_err(|_| invalid())?;
    let revisions = member.workspace.doc_history(&id, HISTORY).await?;
    Ok(Json(DocHistory { revisions }))
}

/// `GET /api/workspaces/{workspace}/docs/{doc}/revisions/{version}`: the doc as it was at
/// `version`.
pub(crate) async fn revision(
    member: Member,
    path: Result<Path<(WorkspaceId, DocId, u64)>, PathRejection>,
) -> Result<Json<DocRecord>, Refused> {
    let Path((_, id, version)) = path.map_err(|_| invalid())?;
    let at = Some(Revision::new(version));
    Ok(Json(record(member.workspace.doc(&id, at).await?)))
}

fn text(content: DocContent) -> Result<DocText, Fault> {
    let title = content.title.try_into().map_err(|_| invalid())?;
    DocText::new(title, content.body, content.anchors).map_err(|error| match error {
        DocTextError::TooLarge => too_large(),
        DocTextError::Unpinned => Fault::new(
            code!("doc.unpinned"),
            "The doc links to code it has no pin for",
        ),
        DocTextError::Unlinked => Fault::new(
            code!("doc.unlinked"),
            "The doc's pins don't match its links",
        ),
    })
}

fn record(doc: Doc) -> DocRecord {
    DocRecord {
        id: doc.id,
        version: doc.version.get(),
        content: DocContent {
            title: doc.text.title().as_str().to_owned(),
            body: doc.text.body().to_owned(),
            anchors: doc.text.anchors().cloned(),
        },
    }
}

/// Logs a refused write that found the workspace full: it stays full, since nothing is
/// pruned, so the operator needs to know.
fn warn_if_full(shared: &Shared, member: &Member, fault: &Fault) {
    if *fault.code() == code!("doc.quota_full") {
        let workspace = member.workspace.id().as_str().to_owned();
        shared
            .log
            .warn("doc quota full", &[("workspace_id", workspace.into())]);
    }
}

fn too_large() -> Fault {
    Fault::new(code!("doc.too_large"), "The doc is larger than the limit")
}

fn invalid() -> Fault {
    Fault::new(code!("doc.invalid"), "The doc request isn't valid")
}

/// The doc routes' refusals.
pub(crate) struct Docs;

impl Refusals for Docs {
    const REFUSALS: &'static [(Code, StatusCode, bool)] = &[
        (code!("doc.invalid"), StatusCode::BAD_REQUEST, false),
        (code!("doc.unpinned"), StatusCode::BAD_REQUEST, false),
        (code!("doc.unlinked"), StatusCode::BAD_REQUEST, false),
        (code!("doc.unknown_value"), StatusCode::BAD_REQUEST, false),
        (code!("doc.too_large"), StatusCode::PAYLOAD_TOO_LARGE, false),
        (
            code!("doc.quota_full"),
            StatusCode::PAYLOAD_TOO_LARGE,
            false,
        ),
        (code!("doc.not_found"), StatusCode::NOT_FOUND, false),
        (code!("doc.stale"), StatusCode::CONFLICT, false),
        (code!("doc.last_version"), StatusCode::CONFLICT, false),
    ];
}

type Refused = http::Refused<Docs>;

#[cfg(test)]
mod tests {
    use serde_json::json;
    use weacon_protocol::{Author, CommitId, DocAnchors, DocPin, DocSummary, WorkspaceChange};

    use super::*;
    use crate::test_support::{Client, Fixture, refusal, workspace_id};

    impl Client {
        async fn ok<T: serde::de::DeserializeOwned>(
            &self,
            head: &str,
            body: &str,
            want: &str,
        ) -> T {
            let (status, body) = self.send(head, body).await;
            assert_eq!(status, want, "{head}: {body}");
            serde_json::from_str(&body).unwrap()
        }

        async fn create_doc(&self, content: &DocContent) -> DocRecord {
            let body = serde_json::to_string(content).unwrap();
            self.ok(
                "POST /api/workspaces/w1/docs",
                &body,
                "http/1.1 201 created",
            )
            .await
        }

        async fn revise(&self, doc: &DocRecord, content: &DocContent) -> (String, String) {
            let revise = DocRevise {
                version: doc.version,
                content: content.clone(),
            };
            let head = format!("POST /api/workspaces/w1/docs/{}", doc.id.as_str());
            self.send(&head, &serde_json::to_string(&revise).unwrap())
                .await
        }

        async fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> T {
            self.ok(
                &format!("GET /api/workspaces/w1/docs{path}"),
                "",
                "http/1.1 200 ok",
            )
            .await
        }
    }

    fn content(title: &str, body: &str, pinned: &[&str]) -> DocContent {
        DocContent {
            title: title.to_owned(),
            body: body.to_owned(),
            anchors: (!pinned.is_empty()).then(|| DocAnchors {
                commit: CommitId::try_from("e".repeat(40)).unwrap(),
                pins: pinned
                    .iter()
                    .map(|anchor| DocPin::try_from(format!("{anchor}#0123456789abcdef")).unwrap())
                    .collect(),
            }),
        }
    }

    fn linked() -> DocContent {
        content(
            "Framing",
            "See [encode](sym:a::encode).",
            &["sym:a::encode"],
        )
    }

    #[tokio::test]
    async fn a_doc_is_made_listed_and_read_and_the_feed_hears_of_it() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let mut changes = fixture.shared.changes.subscribe();

        let made = ada.create_doc(&linked()).await;
        let plain = ada.create_doc(&content("Agents", "Plain.", &[])).await;

        assert_eq!((made.version, &made.content), (1, &linked()));
        assert_eq!(changes.try_recv().unwrap(), workspace_id("w1"));
        let list: DocList = ada.get("").await;
        let summary = |doc: &DocRecord| DocSummary {
            id: doc.id.clone(),
            title: doc.content.title.clone(),
            version: doc.version,
        };
        assert_eq!(list.docs, [summary(&plain), summary(&made)]);
        let read: DocRecord = ada.get(&format!("/{}", made.id.as_str())).await;
        assert_eq!(read, made);
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let stored = w1.changes_since(0, 10).await.unwrap();
        let made_change = WorkspaceChange::DocChanged {
            doc: made.id,
            version: 1,
        };
        assert!(stored.iter().any(|update| update.change == made_change));
    }

    #[tokio::test]
    async fn a_revision_answers_the_doc_at_its_next_version_and_keeps_the_old_one() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let first = ada.create_doc(&content("Framing", "Draft.", &[])).await;

        let (status, body) = ada.revise(&first, &linked()).await;
        let (stale, stale_body) = ada.revise(&first, &content("Late", "Late.", &[])).await;

        assert_eq!(status, "http/1.1 200 ok", "{body}");
        let second: DocRecord = serde_json::from_str(&body).unwrap();
        assert_eq!((second.version, &second.content), (2, &linked()));
        assert_eq!(
            (stale.as_str(), refusal(&stale_body).as_str()),
            ("http/1.1 409 conflict", "doc.stale")
        );
        let id = first.id.as_str();
        assert_eq!(ada.get::<DocRecord>(&format!("/{id}")).await, second);
        assert_eq!(
            ada.get::<DocRecord>(&format!("/{id}/revisions/1")).await,
            first
        );
        assert_eq!(
            ada.get::<DocRecord>(&format!("/{id}/revisions/2")).await,
            second
        );
        let history: DocHistory = ada.get(&format!("/{id}/revisions")).await;
        let versions: Vec<_> = history
            .revisions
            .iter()
            .map(|revision| revision.version)
            .collect();
        assert_eq!(versions, [2, 1]);
        let ada_id = history.revisions.first().unwrap().accepted_by.clone();
        assert!(history.revisions.iter().all(|revision| revision.author
            == Author::Person {
                member: ada_id.clone()
            }));
    }

    #[tokio::test]
    async fn a_doc_or_version_there_isnt_is_not_found() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let doc = ada.create_doc(&linked()).await;
        let id = doc.id.as_str();
        let gone = DocRecord {
            id: DocId::try_from("d-gone".to_owned()).unwrap(),
            ..doc.clone()
        };

        for path in [
            "/d-gone".to_owned(),
            "/d-gone/revisions".to_owned(),
            format!("/{id}/revisions/2"),
            format!("/{id}/revisions/0"),
            format!("/{id}/revisions/{}", u64::MAX),
        ] {
            let (status, body) = ada
                .send(&format!("GET /api/workspaces/w1/docs{path}"), "")
                .await;
            assert_eq!(
                (status.as_str(), refusal(&body).as_str()),
                ("http/1.1 404 not found", "doc.not_found"),
                "{path}"
            );
        }
        let (status, body) = ada.revise(&gone, &linked()).await;
        assert_eq!(
            (status.as_str(), refusal(&body).as_str()),
            ("http/1.1 404 not found", "doc.not_found")
        );
    }

    #[tokio::test]
    async fn bad_or_mismatched_content_is_refused_and_nothing_changes() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let doc = ada.create_doc(&linked()).await;
        let huge = content("Huge", &"a".repeat(300 * 1024), &[]);

        for (content, status, code) in [
            (
                content(" padded", "", &[]),
                "http/1.1 400 bad request",
                "doc.invalid",
            ),
            (
                content("Framing", "[a](sym:a)", &[]),
                "http/1.1 400 bad request",
                "doc.unpinned",
            ),
            (
                content("Framing", "Plain.", &["sym:a"]),
                "http/1.1 400 bad request",
                "doc.unlinked",
            ),
            (huge, "http/1.1 413 payload too large", "doc.too_large"),
        ] {
            let body = serde_json::to_string(&content).unwrap();
            let (made, made_body) = ada.send("POST /api/workspaces/w1/docs", &body).await;
            let (revised, revised_body) = ada.revise(&doc, &content).await;
            let got = [
                (made, refusal(&made_body)),
                (revised, refusal(&revised_body)),
            ];
            let want = (status.to_owned(), code.to_owned());
            assert_eq!(got, [want.clone(), want]);
        }
        assert_eq!(ada.get::<DocList>("").await.docs.len(), 1);
        assert_eq!(
            ada.get::<DocRecord>(&format!("/{}", doc.id.as_str())).await,
            doc
        );
    }

    #[tokio::test]
    async fn a_request_that_isnt_a_doc_request_is_invalid() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let doc = ada.create_doc(&linked()).await;
        let head = format!("POST /api/workspaces/w1/docs/{}", doc.id.as_str());

        for (head, body) in [
            ("POST /api/workspaces/w1/docs", "{".to_owned()),
            (head.as_str(), json!({ "content": linked() }).to_string()),
            (
                "POST /api/workspaces/w1/docs/a%20b",
                json!({ "version": 1, "content": linked() }).to_string(),
            ),
            ("GET /api/workspaces/w1/docs/d1/revisions/x", String::new()),
        ] {
            let (status, answer) = ada.send(head, &body).await;
            assert_eq!(
                (status.as_str(), refusal(&answer).as_str()),
                ("http/1.1 400 bad request", "doc.invalid"),
                "{head}"
            );
        }
        let list: DocList = ada.get("").await;
        assert_eq!(list.docs.len(), 1);
        assert_eq!(
            ada.get::<DocRecord>(&format!("/{}", doc.id.as_str())).await,
            doc
        );
    }

    #[tokio::test]
    async fn a_doc_larger_than_other_requests_or_heavy_with_escapes_fits() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let large = content("Large", &"a".repeat(100 * 1024), &[]);
        let escaped = content("Escaped", &"\u{1}".repeat(256 * 1024), &[]);

        let made = ada.create_doc(&large).await;
        let (status, body) = ada.revise(&made, &escaped).await;

        assert_eq!(status, "http/1.1 200 ok", "{body}");
        let revised: DocRecord = serde_json::from_str(&body).unwrap();
        assert_eq!((made.content, revised.content), (large, escaped));
    }

    #[tokio::test]
    async fn a_body_over_the_route_limit_is_too_large() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let body = serde_json::to_string(&content("Huge", &"a".repeat(MAX_BODY), &[])).unwrap();

        let (status, answer) = ada.send("POST /api/workspaces/w1/docs", &body).await;

        assert_eq!(
            (status.as_str(), refusal(&answer).as_str()),
            ("http/1.1 413 payload too large", "doc.too_large")
        );
    }

    #[tokio::test]
    async fn only_a_signed_in_member_reaches_the_docs() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        fixture
            .shared
            .db
            .create_workspace(&workspace_id("w2"), "Other")
            .await
            .unwrap();
        let stranger = Client {
            address: ada.address,
            auth: String::new(),
        };
        let body = serde_json::to_string(&linked()).unwrap();
        for (who, head) in [
            (&stranger, "GET /api/workspaces/w1/docs"),
            (&stranger, "POST /api/workspaces/w1/docs"),
            (&ada, "GET /api/workspaces/w2/docs"),
            (&ada, "POST /api/workspaces/w2/docs"),
            (&ada, "GET /api/workspaces/w2/docs/d1"),
            (&ada, "POST /api/workspaces/w2/docs/d1"),
            (&ada, "GET /api/workspaces/w2/docs/d1/revisions"),
            (&ada, "GET /api/workspaces/w2/docs/d1/revisions/1"),
        ] {
            let (status, answer) = who.send(head, &body).await;
            assert_eq!(
                (status.as_str(), refusal(&answer).as_str()),
                ("http/1.1 401 unauthorized", "sign_in.required"),
                "{head}"
            );
        }
        assert!(ada.get::<DocList>("").await.docs.is_empty());
    }

    #[test]
    fn a_failure_of_the_server_is_not_a_refusal() {
        let broken = Fault::new(code!("db.failed"), "The database failed");

        let answer = Refused::from(broken).into_response();

        assert_eq!(answer.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn a_full_workspace_takes_no_more_docs_and_the_operator_hears_of_it() {
        let fixture = Fixture::new().await;
        let (_server, ada) = fixture.client("ada@example.com").await;
        let doc = ada.create_doc(&linked()).await;
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        w1.fill_doc_quota().await;
        let body = serde_json::to_string(&linked()).unwrap();

        let (made, made_body) = ada.send("POST /api/workspaces/w1/docs", &body).await;
        let (revised, revised_body) = ada.revise(&doc, &linked()).await;

        let full = (
            "http/1.1 413 payload too large".to_owned(),
            "doc.quota_full".to_owned(),
        );
        let got = [
            (made, refusal(&made_body)),
            (revised, refusal(&revised_body)),
        ];
        assert_eq!(got, [full.clone(), full]);
        assert_eq!(
            ada.get::<DocRecord>(&format!("/{}", doc.id.as_str())).await,
            doc
        );
        let records = fixture.records.lock().unwrap().clone();
        let warned: Vec<_> = records
            .iter()
            .filter(|record| record.msg == "doc quota full")
            .map(|record| record.fields["workspace_id"].clone())
            .collect();
        assert_eq!(warned, ["w1", "w1"]);
    }
}
