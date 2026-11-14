use axum::Json;
use axum::extract::rejection::PathRejection;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use weacon_model::{Attribution, OrderKey, Revision, Task, TaskEdit, Title};
use weacon_observability::Fault;
use weacon_protocol::{
    Code, TaskChange, TaskCreate, TaskEditBody, TaskId, TaskList, TaskRecord, WorkspaceId, code,
};

use crate::app::new_id;
use crate::auth::Member;
use crate::db::task_not_found;
use crate::http::{Body, Shared, Unreadable, error, failure};

/// `GET /api/workspaces/{workspace}/tasks`: the workspace's tasks, in their order.
pub(crate) async fn list(member: Member) -> Response {
    match member.workspace.tasks().await {
        Ok(tasks) => Json(TaskList {
            tasks: tasks.into_iter().map(record).collect(),
        })
        .into_response(),
        Err(fault) => answer(fault),
    }
}

/// `POST /api/workspaces/{workspace}/tasks`: makes a task, to do, in front of `before` or
/// last.
pub(crate) async fn create(
    State(shared): State<Shared>,
    member: Member,
    body: Result<Body<TaskCreate>, Unreadable>,
) -> Response {
    let made = async {
        let body = parsed(body)?;
        let title = title(body.title)?;
        let tasks = member.workspace.tasks().await?;
        let order = place(&tasks, None, body.before.as_ref())?;
        let id = TaskId::try_from(new_id()).map_err(|_| invalid())?;
        let task = Task::new(id, title, order);
        let attribution = Attribution::by(member.id.clone(), shared.now());
        member.workspace.add_task(&task, &attribution).await?;
        Ok(task)
    };
    match made.await {
        Ok(task) => {
            shared.changed(member.workspace.id());
            (StatusCode::CREATED, Json(record(task))).into_response()
        }
        Err(fault) => answer(fault),
    }
}

/// `POST /api/workspaces/{workspace}/tasks/{task}`: makes one edit to a task the client read
/// at `version`.
pub(crate) async fn change(
    State(shared): State<Shared>,
    member: Member,
    path: Result<Path<(WorkspaceId, TaskId)>, PathRejection>,
    body: Result<Body<TaskChange>, Unreadable>,
) -> Response {
    let edited = async {
        let Path((_, id)) = path.map_err(|_| invalid())?;
        let body = parsed(body)?;
        let edit = match body.edit {
            TaskEditBody::Title { title: text } => TaskEdit::Title(title(text)?),
            TaskEditBody::Status { status } => TaskEdit::Status(status),
            TaskEditBody::Move { before } => {
                let tasks = member.workspace.tasks().await?;
                TaskEdit::Order(place(&tasks, Some(&id), before.as_ref())?)
            }
            TaskEditBody::Newer => return Err(invalid()),
        };
        let attribution = Attribution::by(member.id.clone(), shared.now());
        let read_at = Revision::new(body.version);
        member
            .workspace
            .edit_task(&id, read_at, edit, &attribution)
            .await
    };
    match edited.await {
        Ok(task) => {
            shared.changed(member.workspace.id());
            Json(record(task)).into_response()
        }
        Err(fault) => answer(fault),
    }
}

/// An order key in front of `before`, or after the last task, among the tasks but `moving`.
fn place(
    tasks: &[Task],
    moving: Option<&TaskId>,
    before: Option<&TaskId>,
) -> Result<OrderKey, Fault> {
    if before.is_some() && before == moving {
        return Err(invalid());
    }
    let others: Vec<&Task> = tasks
        .iter()
        .filter(|task| Some(&task.id) != moving)
        .collect();
    let at = match before {
        Some(before) => others
            .iter()
            .position(|task| task.id == *before)
            .ok_or_else(task_not_found)?,
        None => others.len(),
    };
    let after = at.checked_sub(1).and_then(|at| others.get(at));
    let lo = after.map(|task| &task.order);
    let hi = others.get(at).map(|task| &task.order);
    OrderKey::between(lo, hi)
        .ok_or_else(|| Fault::new(code!("task.no_room"), "There is no room for the task there"))
}

fn title(text: String) -> Result<Title, Fault> {
    Title::try_from(text).map_err(|_| invalid())
}

fn record(task: Task) -> TaskRecord {
    TaskRecord {
        id: task.id,
        title: task.title.as_str().to_owned(),
        status: task.status,
        order: task.order.as_str().to_owned(),
        version: task.version.get(),
    }
}

/// A request's body, or `task.too_large` past the size limit and `task.invalid` otherwise.
fn parsed<T>(body: Result<Body<T>, Unreadable>) -> Result<T, Fault> {
    match body {
        Ok(Body(body)) => Ok(body),
        Err(Unreadable::TooLarge) => Err(Fault::new(
            code!("task.too_large"),
            "The task request is too large",
        )),
        Err(_) => Err(invalid()),
    }
}

fn invalid() -> Fault {
    Fault::new(code!("task.invalid"), "The task request isn't valid")
}

/// The refusals the task routes answer, with their status; anything else is the server failing.
const REFUSALS: [(Code, StatusCode); 8] = [
    (code!("task.invalid"), StatusCode::BAD_REQUEST),
    (code!("task.unknown_value"), StatusCode::BAD_REQUEST),
    (code!("task.not_found"), StatusCode::NOT_FOUND),
    (code!("task.stale"), StatusCode::CONFLICT),
    (code!("task.no_room"), StatusCode::CONFLICT),
    (code!("task.last_version"), StatusCode::CONFLICT),
    (code!("task.too_many"), StatusCode::CONFLICT),
    (code!("task.too_large"), StatusCode::PAYLOAD_TOO_LARGE),
];

/// Answers a refusal with its status, and anything else as the server failing.
fn answer(fault: Fault) -> Response {
    let code = fault.code().clone();
    // Another change took the same place at the same moment; reading again finds a free one.
    if code == code!("db.exists") {
        return error(StatusCode::CONFLICT, code, true);
    }
    match REFUSALS.iter().find(|(refusal, _)| *refusal == code) {
        Some((_, status)) => error(*status, code, false),
        None => failure(fault),
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use weacon_protocol::{ErrorBody, WorkspaceChange};

    use weacon_protocol::TaskStatus;

    use super::*;
    use crate::test_support::{
        Fixture, Running, bearer, refusal, request_with, status, workspace_id,
    };

    struct Ada {
        address: SocketAddr,
        auth: String,
    }

    impl Ada {
        async fn send(&self, head: &str, body: &str) -> (String, String) {
            let (head, body) = request_with(self.address, head, &self.auth, body).await;
            (status(&head).to_owned(), body)
        }

        async fn list(&self) -> Vec<TaskRecord> {
            let (status, body) = self.send("GET /api/workspaces/w1/tasks", "").await;
            assert_eq!(status, "http/1.1 200 ok");
            serde_json::from_str::<TaskList>(&body).unwrap().tasks
        }

        async fn create(&self, title: &str, before: Option<&TaskRecord>) -> TaskRecord {
            let create = TaskCreate {
                title: title.to_owned(),
                before: before.map(|task| task.id.clone()),
            };
            let body = serde_json::to_string(&create).unwrap();
            let (status, body) = self.send("POST /api/workspaces/w1/tasks", &body).await;
            assert_eq!(status, "http/1.1 201 created");
            serde_json::from_str(&body).unwrap()
        }

        async fn change(&self, task: &TaskRecord, edit: TaskEditBody) -> (String, String) {
            let change = TaskChange {
                version: task.version,
                edit,
            };
            let head = format!("POST /api/workspaces/w1/tasks/{}", task.id.as_str());
            self.send(&head, &serde_json::to_string(&change).unwrap())
                .await
        }

        async fn changed(&self, task: &TaskRecord, edit: TaskEditBody) -> TaskRecord {
            let (status, body) = self.change(task, edit).await;
            assert_eq!(status, "http/1.1 200 ok", "{body}");
            serde_json::from_str(&body).unwrap()
        }
    }

    /// The running server, which stops once dropped, and ada signed in to it.
    async fn ada(fixture: &Fixture) -> (Running, Ada) {
        let (server, body) = fixture.signed_in("ada@example.com").await;
        let ada = Ada {
            address: server.address,
            auth: bearer(&body),
        };
        (server, ada)
    }

    fn titles(tasks: &[TaskRecord]) -> Vec<&str> {
        tasks.iter().map(|task| task.title.as_str()).collect()
    }

    #[tokio::test]
    async fn new_tasks_go_last_or_in_front_of_one_and_the_feed_hears_of_each() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        let mut changes = fixture.shared.changes.subscribe();

        let write = ada.create("Write", None).await;
        let ship = ada.create("Ship", None).await;
        let plan = ada.create("Plan", Some(&write)).await;
        let review = ada.create("Review", Some(&ship)).await;

        assert_eq!(
            titles(&ada.list().await),
            ["Plan", "Write", "Review", "Ship"]
        );
        let made: Vec<_> = [&write, &ship, &plan, &review]
            .map(|task| WorkspaceChange::TaskChanged {
                task: task.id.clone(),
                version: 1,
            })
            .into();
        assert_eq!(ada.list().await, [plan, write.clone(), review, ship]);
        assert_eq!((write.status, write.version), (TaskStatus::Todo, 1));
        assert_eq!(changes.try_recv().unwrap(), workspace_id("w1"));
        let w1 = fixture
            .shared
            .db
            .workspace(&workspace_id("w1"))
            .await
            .unwrap();
        let stored = w1.changes_since(0, 10).await.unwrap();
        let stored: Vec<_> = stored.into_iter().map(|update| update.change).collect();
        assert_eq!(stored, made);
    }

    #[tokio::test]
    async fn an_edit_answers_the_task_at_its_next_version() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        let write = ada.create("Write", None).await;
        let ship = ada.create("Ship", None).await;

        let renamed = ada
            .changed(
                &write,
                TaskEditBody::Title {
                    title: "Draft".to_owned(),
                },
            )
            .await;
        let done = ada
            .changed(
                &renamed,
                TaskEditBody::Status {
                    status: TaskStatus::Done,
                },
            )
            .await;
        let moved = ada
            .changed(&done, TaskEditBody::Move { before: None })
            .await;
        let back = ada
            .changed(
                &moved,
                TaskEditBody::Move {
                    before: Some(ship.id.clone()),
                },
            )
            .await;

        assert_eq!((renamed.title.as_str(), renamed.version), ("Draft", 2));
        assert_eq!((done.status, done.version), (TaskStatus::Done, 3));
        assert_eq!(titles(&ada.list().await), ["Draft", "Ship"]);
        assert_eq!(back.version, 5);
        assert!(moved.order > ship.order && back.order < ship.order);
    }

    #[tokio::test]
    async fn a_stale_or_missing_task_is_refused_and_nothing_changes() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        let write = ada.create("Write", None).await;
        ada.changed(
            &write,
            TaskEditBody::Title {
                title: "Draft".to_owned(),
            },
        )
        .await;
        let gone = TaskRecord {
            id: TaskId::try_from("t-gone".to_owned()).unwrap(),
            ..write.clone()
        };

        let (stale, stale_body) = ada
            .change(
                &write,
                TaskEditBody::Title {
                    title: "Late".to_owned(),
                },
            )
            .await;
        let (missing, missing_body) = ada
            .change(
                &gone,
                TaskEditBody::Title {
                    title: "Late".to_owned(),
                },
            )
            .await;
        let (nowhere, nowhere_body) = ada
            .change(
                &write,
                TaskEditBody::Move {
                    before: Some(gone.id.clone()),
                },
            )
            .await;

        assert_eq!(stale, "http/1.1 409 conflict");
        assert_eq!(refusal(&stale_body), "task.stale");
        assert_eq!(missing, "http/1.1 404 not found");
        assert_eq!(refusal(&missing_body), "task.not_found");
        assert_eq!(nowhere, "http/1.1 404 not found");
        assert_eq!(refusal(&nowhere_body), "task.not_found");
        assert_eq!(titles(&ada.list().await), ["Draft"]);
    }

    #[tokio::test]
    async fn a_body_past_the_size_limit_is_too_large_and_one_under_it_is_read() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        let write = ada.create("Write", None).await;
        let huge = TaskCreate {
            title: "x".repeat(20_000),
            before: None,
        };
        let huge = serde_json::to_string(&huge).unwrap();
        let change = format!("POST /api/workspaces/w1/tasks/{}", write.id.as_str());

        let ship = TaskCreate {
            title: "Ship".to_owned(),
            before: None,
        };
        // Spaces are JSON too, so this body is just under the limit and still read.
        let padded = format!("{:<16000}", serde_json::to_string(&ship).unwrap());

        for head in ["POST /api/workspaces/w1/tasks", change.as_str()] {
            let (status, body) = ada.send(head, &huge).await;
            assert_eq!(status, "http/1.1 413 payload too large", "{head}");
            assert_eq!(refusal(&body), "task.too_large");
        }
        assert_eq!(ada.list().await, [write]);
        let (status, _) = ada.send("POST /api/workspaces/w1/tasks", &padded).await;
        assert_eq!(status, "http/1.1 201 created");
    }

    #[tokio::test]
    async fn bad_input_is_refused_as_a_bad_request() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        let write = ada.create("Write", None).await;
        let current = TaskRecord {
            version: 1,
            ..write.clone()
        };
        let edits = [
            TaskEditBody::Title {
                title: " padded".to_owned(),
            },
            TaskEditBody::Title {
                title: String::new(),
            },
            TaskEditBody::Move {
                before: Some(write.id.clone()),
            },
        ];
        for edit in edits {
            let (status, body) = ada.change(&current, edit).await;
            assert_eq!(status, "http/1.1 400 bad request");
            assert_eq!(refusal(&body), "task.invalid");
        }
        for (head, body) in [
            (
                "POST /api/workspaces/w1/tasks",
                r#"{"title":"line\nbreak"}"#,
            ),
            ("POST /api/workspaces/w1/tasks", "{"),
            (
                "POST /api/workspaces/w1/tasks/a%20b",
                r#"{"version":1,"edit":{"kind":"move"}}"#,
            ),
            (
                "POST /api/workspaces/w1/tasks/x",
                r#"{"version":1,"edit":{"kind":"archive"}}"#,
            ),
        ] {
            let (status, body) = ada.send(head, body).await;
            assert_eq!(status, "http/1.1 400 bad request", "{head}");
            assert_eq!(refusal(&body), "task.invalid");
        }
        let later = r#"{"version":1,"edit":{"kind":"status","status":"later"}}"#;
        let head = format!("POST /api/workspaces/w1/tasks/{}", write.id.as_str());
        let (status, body) = ada.send(&head, later).await;
        assert_eq!(status, "http/1.1 400 bad request");
        assert_eq!(refusal(&body), "task.unknown_value");
        assert_eq!(ada.list().await, [write]);
    }

    #[tokio::test]
    async fn only_a_signed_in_member_reaches_the_tasks() {
        let fixture = Fixture::new().await;
        let (_server, ada) = ada(&fixture).await;
        fixture
            .shared
            .db
            .create_workspace(&workspace_id("w2"), "Other")
            .await
            .unwrap();
        let stranger = Ada {
            address: ada.address,
            auth: String::new(),
        };

        for (who, head) in [
            (&stranger, "GET /api/workspaces/w1/tasks"),
            (&stranger, "POST /api/workspaces/w1/tasks"),
            (&ada, "GET /api/workspaces/w2/tasks"),
            (&ada, "POST /api/workspaces/w2/tasks"),
            (&ada, "POST /api/workspaces/w2/tasks/t1"),
        ] {
            let (status, body) = who.send(head, r#"{"title":"Spy"}"#).await;
            assert_eq!(status, "http/1.1 401 unauthorized", "{head}");
            assert_eq!(refusal(&body), "sign_in.required");
        }
        assert!(ada.list().await.is_empty());
    }

    #[tokio::test]
    async fn a_full_workspace_or_a_taken_place_is_a_conflict() {
        for (fault, retryable) in [
            (Fault::new(code!("db.exists"), "It already exists"), true),
            (Fault::new(code!("task.too_many"), "Full"), false),
        ] {
            let code = fault.code().as_str().to_owned();

            let answer = answer(fault);

            assert_eq!(answer.status(), StatusCode::CONFLICT, "{code}");
            let body = axum::body::to_bytes(answer.into_body(), 1024)
                .await
                .unwrap();
            let error: ErrorBody = serde_json::from_slice(&body).unwrap();
            assert_eq!(
                (error.code.as_str(), error.retryable),
                (code.as_str(), retryable)
            );
        }
    }
}
