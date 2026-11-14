use sqlx::SqliteConnection;
use weacon_model::{Change, EditError, Revision, Task, TaskEdit};
use weacon_observability::Fault;
use weacon_protocol::{Author, MemberId, TaskId, TaskStatus, code};

use super::{Workspace, failed, stored, unreadable};

type Row = (String, String, String, String, i64);

impl Workspace {
    /// Stores a new task, made by `change`. An id or order key already in this workspace is
    /// `db.exists`.
    pub(crate) async fn add_task(&self, task: &Task, change: &Change) -> Result<(), Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        sqlx::query(
            "INSERT INTO tasks (workspace_id, task_id, title, status, order_key, version)
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(task.id.as_str())
        .bind(task.title.as_str())
        .bind(status_text(task.status)?)
        .bind(task.order.as_str())
        .bind(version_number(task.version)?)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.record(&mut transaction, task, change).await?;
        transaction.commit().await.map_err(failed)
    }

    /// The tasks, in their order.
    pub(crate) async fn tasks(&self) -> Result<Vec<Task>, Fault> {
        sqlx::query_as::<_, Row>(
            "SELECT task_id, title, status, order_key, version FROM tasks
             WHERE workspace_id = ? ORDER BY order_key",
        )
        .bind(self.id.as_str())
        .fetch_all(&self.db.readers)
        .await
        .map_err(failed)?
        .into_iter()
        .map(stored_task)
        .collect()
    }

    /// Makes `edit` to the task `id` and gives the task as it then is. One changed since
    /// `read_at` is `task.stale`, one there isn't is `task.not_found`, and an order key
    /// another task has is `db.exists`.
    pub(crate) async fn edit_task(
        &self,
        id: &TaskId,
        read_at: Revision,
        edit: TaskEdit,
        change: &Change,
    ) -> Result<Task, Fault> {
        let mut transaction = self
            .db
            .writer
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(failed)?;
        let row = sqlx::query_as::<_, Row>(
            "SELECT task_id, title, status, order_key, version FROM tasks
             WHERE workspace_id = ? AND task_id = ?",
        )
        .bind(self.id.as_str())
        .bind(id.as_str())
        .fetch_optional(&mut *transaction)
        .await
        .map_err(failed)?
        .ok_or_else(task_not_found)?;
        let edited = stored_task(row)?.edit(read_at, edit).map_err(refused)?;
        sqlx::query(
            "UPDATE tasks SET title = ?, status = ?, order_key = ?, version = ?
             WHERE workspace_id = ? AND task_id = ?",
        )
        .bind(edited.title.as_str())
        .bind(status_text(edited.status)?)
        .bind(edited.order.as_str())
        .bind(version_number(edited.version)?)
        .bind(self.id.as_str())
        .bind(id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.record(&mut transaction, &edited, change).await?;
        transaction.commit().await.map_err(failed)?;
        Ok(edited)
    }

    /// Records who is behind the version `task` is now at, and the change for the feed.
    async fn record(
        &self,
        transaction: &mut SqliteConnection,
        task: &Task,
        change: &Change,
    ) -> Result<(), Fault> {
        let (kind, author) = author_columns(change.author())?;
        let version = version_number(task.version)?;
        sqlx::query(
            "INSERT INTO task_versions
                 (workspace_id, task_id, version, author_kind, author_member, accepted_by, at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(task.id.as_str())
        .bind(version)
        .bind(kind)
        .bind(author.as_str())
        .bind(change.accepted_by().as_str())
        .bind(change.at())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.task_changed(transaction, &task.id, version, change.accepted_by())
            .await
    }
}

fn stored_task((id, title, status, order, version): Row) -> Result<Task, Fault> {
    Ok(Task {
        id: stored(id)?,
        title: stored(title)?,
        status: stored_status(&status)?,
        order: stored(order)?,
        version: u64::try_from(version)
            .map(Revision::new)
            .map_err(|_| unreadable())?,
    })
}

pub(crate) fn task_not_found() -> Fault {
    Fault::new(code!("task.not_found"), "There is no such task")
}

fn refused(error: EditError) -> Fault {
    match error {
        EditError::Stale { .. } => Fault::new(
            code!("task.stale"),
            "The task has changed since it was read",
        ),
        EditError::UnknownStatus => unknown(),
        EditError::LastVersion => last_version(),
    }
}

/// A status or an author from a newer peer, which this build can't store.
fn unknown() -> Fault {
    Fault::new(
        code!("task.unknown_value"),
        "The task has a value this version doesn't know",
    )
}

fn last_version() -> Fault {
    Fault::new(
        code!("task.last_version"),
        "The task can't be changed any more",
    )
}

fn version_number(version: Revision) -> Result<i64, Fault> {
    i64::try_from(version.get()).map_err(|_| last_version())
}

fn status_text(status: TaskStatus) -> Result<&'static str, Fault> {
    match status {
        TaskStatus::Todo => Ok("todo"),
        TaskStatus::InProgress => Ok("in_progress"),
        TaskStatus::Done => Ok("done"),
        TaskStatus::Newer => Err(unknown()),
    }
}

fn stored_status(text: &str) -> Result<TaskStatus, Fault> {
    match text {
        "todo" => Ok(TaskStatus::Todo),
        "in_progress" => Ok(TaskStatus::InProgress),
        "done" => Ok(TaskStatus::Done),
        _ => Err(unreadable()),
    }
}

fn author_columns(author: &Author) -> Result<(&'static str, &MemberId), Fault> {
    match author {
        Author::Person { member } => Ok(("person", member)),
        Author::Agent { owner } => Ok(("agent", owner)),
        Author::Newer => Err(unknown()),
    }
}

#[cfg(test)]
mod tests {
    use weacon_model::{OrderKey, Title};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{member_id, two_teams};

    const NOW: i64 = 1_762_077_600;

    fn new_task(id: &str, title: &str, order: &str) -> Task {
        Task::new(
            TaskId::try_from(id.to_owned()).unwrap(),
            Title::try_from(title.to_owned()).unwrap(),
            OrderKey::try_from(order.to_owned()).unwrap(),
        )
    }

    fn by(member: &str) -> Change {
        Change::by(member_id(member), NOW)
    }

    fn rename(title: &str) -> TaskEdit {
        TaskEdit::Title(Title::try_from(title.to_owned()).unwrap())
    }

    fn code(fault: &Fault) -> &str {
        fault.code().as_str()
    }

    async fn changes(workspace: &Workspace) -> Vec<(String, i64, String, String, String, i64)> {
        sqlx::query_as(
            "SELECT task_id, version, author_kind, author_member, accepted_by, at
             FROM task_versions WHERE workspace_id = ? ORDER BY task_id, version",
        )
        .bind(workspace.id.as_str())
        .fetch_all(&workspace.db.readers)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn tasks_are_listed_in_their_order() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let started = Task {
            status: TaskStatus::InProgress,
            ..new_task("t2", "Third", "b")
        };
        for task in [
            new_task("t1", "Second", "V"),
            started.clone(),
            new_task("t3", "First", "B"),
        ] {
            one.add_task(&task, &by("m1")).await.unwrap();
        }

        assert_eq!(
            one.tasks().await.unwrap(),
            [
                new_task("t3", "First", "B"),
                new_task("t1", "Second", "V"),
                started,
            ]
        );
    }

    #[tokio::test]
    async fn an_edit_is_stored_at_the_next_version() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        let id = task.id.clone();

        let renamed = one
            .edit_task(&id, Revision::FIRST, rename("Ship"), &by("m1"))
            .await
            .unwrap();
        let done = one
            .edit_task(
                &id,
                renamed.version,
                TaskEdit::Status(TaskStatus::Done),
                &by("m1"),
            )
            .await
            .unwrap();
        let moved = one
            .edit_task(
                &id,
                done.version,
                TaskEdit::Order(OrderKey::try_from("W".to_owned()).unwrap()),
                &by("m1"),
            )
            .await
            .unwrap();

        let expected = Task {
            status: TaskStatus::Done,
            version: Revision::new(4),
            ..new_task("t1", "Ship", "W")
        };
        assert_eq!(moved, expected);
        assert_eq!(one.tasks().await.unwrap(), [expected]);
    }

    #[tokio::test]
    async fn a_stale_edit_is_refused_and_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        let first = one
            .edit_task(&task.id, Revision::FIRST, rename("Ship"), &by("m1"))
            .await
            .unwrap();

        let late = one
            .edit_task(&task.id, Revision::FIRST, rename("Drop"), &by("m1"))
            .await
            .unwrap_err();

        assert_eq!(code(&late), "task.stale");
        assert_eq!(one.tasks().await.unwrap(), [first]);
        assert_eq!(changes(&one).await.len(), 2);
    }

    #[tokio::test]
    async fn every_version_records_who_made_it_and_who_accepted_it() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        let agents = Change::accepted(member_id("m1"), member_id("m3"), NOW + 5);
        one.edit_task(&task.id, Revision::FIRST, rename("Ship"), &agents)
            .await
            .unwrap();

        let row = |version, kind: &str, accepted: &str, at| {
            (
                "t1".to_owned(),
                version,
                kind.to_owned(),
                "m1".to_owned(),
                accepted.to_owned(),
                at,
            )
        };
        assert_eq!(
            changes(&one).await,
            [row(1, "person", "m1", NOW), row(2, "agent", "m3", NOW + 5)]
        );
    }

    #[tokio::test]
    async fn workspaces_keep_their_tasks_apart() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        one.add_task(&new_task("t1", "Ours", "V"), &by("m1"))
            .await
            .unwrap();
        two.add_task(&new_task("t1", "Theirs", "V"), &by("m2"))
            .await
            .unwrap();
        let id = TaskId::try_from("t1".to_owned()).unwrap();

        let edited = two
            .edit_task(&id, Revision::FIRST, rename("Changed"), &by("m2"))
            .await
            .unwrap();

        assert_eq!(one.tasks().await.unwrap(), [new_task("t1", "Ours", "V")]);
        assert_eq!(two.tasks().await.unwrap(), [edited]);
        assert_eq!(changes(&one).await.len(), 1);
        assert_eq!(changes(&two).await.len(), 2);
    }

    #[tokio::test]
    async fn another_workspaces_task_or_member_cant_be_used() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        let task = new_task("t1", "Ours", "V");
        one.add_task(&task, &by("m1")).await.unwrap();

        let missing = two
            .edit_task(&task.id, Revision::FIRST, rename("Taken"), &by("m2"))
            .await
            .unwrap_err();
        let stranger = one
            .edit_task(&task.id, Revision::FIRST, rename("Taken"), &by("m2"))
            .await
            .unwrap_err();
        let added = one
            .add_task(&new_task("t2", "Theirs", "W"), &by("m2"))
            .await
            .unwrap_err();

        assert_eq!(code(&missing), "task.not_found");
        assert_eq!(code(&stranger), "db.failed");
        assert_eq!(code(&added), "db.failed");
        assert_eq!(one.tasks().await.unwrap(), [task]);
        assert_eq!(two.tasks().await.unwrap(), []);
        assert_eq!(changes(&one).await.len(), 1);
    }

    #[tokio::test]
    async fn an_id_or_an_order_key_is_used_once_in_a_workspace() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        one.add_task(&new_task("t2", "Read", "W"), &by("m1"))
            .await
            .unwrap();

        let same_id = one
            .add_task(&new_task("t1", "Again", "X"), &by("m1"))
            .await
            .unwrap_err();
        let same_key = one
            .add_task(&new_task("t3", "Again", "V"), &by("m1"))
            .await
            .unwrap_err();
        let moved_onto = one
            .edit_task(
                &task.id,
                Revision::FIRST,
                TaskEdit::Order(OrderKey::try_from("W".to_owned()).unwrap()),
                &by("m1"),
            )
            .await
            .unwrap_err();

        for fault in [same_id, same_key, moved_onto] {
            assert_eq!(code(&fault), "db.exists");
        }
        assert_eq!(one.tasks().await.unwrap().len(), 2);
        assert_eq!(changes(&one).await.len(), 2);
    }

    #[tokio::test]
    async fn values_this_build_cant_store_are_refused() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        let newer = Task {
            status: TaskStatus::Newer,
            ..new_task("t2", "Read", "W")
        };
        let too_far = Task {
            version: Revision::new(u64::MAX),
            ..new_task("t3", "Read", "X")
        };

        let edited = one
            .edit_task(
                &task.id,
                Revision::FIRST,
                TaskEdit::Status(TaskStatus::Newer),
                &by("m1"),
            )
            .await
            .unwrap_err();
        let added = one.add_task(&newer, &by("m1")).await.unwrap_err();
        let last = one.add_task(&too_far, &by("m1")).await.unwrap_err();

        assert_eq!(code(&edited), "task.unknown_value");
        assert_eq!(code(&added), "task.unknown_value");
        assert_eq!(code(&last), "task.last_version");
        assert_eq!(
            code(&author_columns(&Author::Newer).unwrap_err()),
            "task.unknown_value"
        );
        assert_eq!(one.tasks().await.unwrap(), [task]);
    }

    #[tokio::test]
    async fn a_task_at_the_last_version_a_row_holds_cant_be_edited() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        sqlx::query("UPDATE tasks SET version = ?")
            .bind(i64::MAX)
            .execute(&one.db.writer)
            .await
            .unwrap();
        let last = Revision::new(u64::try_from(i64::MAX).unwrap());

        let fault = one
            .edit_task(&task.id, last, rename("Ship"), &by("m1"))
            .await
            .unwrap_err();

        assert_eq!(code(&fault), "task.last_version");
    }

    #[tokio::test]
    async fn stored_values_are_checked_when_read() {
        for (column, value) in [
            ("status", "'blocked'"),
            ("title", "' '"),
            ("order_key", "'V0'"),
            ("version", "-1"),
            ("task_id", "'a b'"),
        ] {
            let dir = TempDir::new().unwrap();
            let (one, _) = two_teams(&dir).await;
            one.add_task(&new_task("t1", "Write", "V"), &by("m1"))
                .await
                .unwrap();
            sqlx::raw_sql("PRAGMA foreign_keys = OFF")
                .execute(&one.db.writer)
                .await
                .unwrap();
            sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
                "UPDATE tasks SET {column} = {value}"
            )))
            .execute(&one.db.writer)
            .await
            .unwrap();

            let fault = one.tasks().await.unwrap_err();
            assert_eq!(code(&fault), "db.unreadable", "{column}");
        }
    }
}
