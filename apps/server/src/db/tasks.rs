use sqlx::SqliteConnection;
use weacon_model::{Attribution, EditError, Revision, Task, TaskEdit};
use weacon_observability::Fault;
use weacon_protocol::{TaskId, TaskStatus, code};

use super::changes::Versioned;
use super::{
    Workspace, author_columns, failed, stored, stored_version, unreadable, version_number,
};

type Row = (String, String, String, String, i64);

/// The most tasks a workspace holds, since making or moving one reads them all.
const MAX_TASKS: u32 = 2_000;

impl Workspace {
    /// Stores a new task, made as `attribution` says. An id or order key already in this
    /// workspace is `db.exists`, and a workspace with [`MAX_TASKS`] already is `task.too_many`.
    pub(crate) async fn add_task(
        &self,
        task: &Task,
        attribution: &Attribution,
    ) -> Result<(), Fault> {
        let mut transaction = self.db.write().await?;
        self.insert_task(&mut transaction, task, attribution)
            .await?;
        transaction.commit().await.map_err(failed)
    }

    /// [`Self::add_task`], inside the caller's transaction.
    pub(super) async fn insert_task(
        &self,
        transaction: &mut SqliteConnection,
        task: &Task,
        attribution: &Attribution,
    ) -> Result<(), Fault> {
        let added = sqlx::query(
            "INSERT INTO tasks (workspace_id, task_id, title, status, order_key, version)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6
             WHERE (SELECT COUNT(*) FROM tasks WHERE workspace_id = ?1) < ?7",
        )
        .bind(self.id.as_str())
        .bind(task.id.as_str())
        .bind(task.title.as_str())
        .bind(status_text(task.status)?)
        .bind(task.order.as_str())
        .bind(version_number(task.version, last_version)?)
        .bind(MAX_TASKS)
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        if added.rows_affected() == 0 {
            return Err(Fault::new(
                code!("task.too_many"),
                "The workspace has as many tasks as it can hold",
            ));
        }
        self.record_version(transaction, task, attribution).await
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
        attribution: &Attribution,
    ) -> Result<Task, Fault> {
        let mut transaction = self.db.write().await?;
        let edited = self
            .update_task(&mut transaction, id, read_at, edit, attribution)
            .await?;
        transaction.commit().await.map_err(failed)?;
        Ok(edited)
    }

    /// The task `id` as it is now, inside the caller's transaction; `task.not_found` when
    /// there's none.
    pub(super) async fn read_task(
        &self,
        transaction: &mut SqliteConnection,
        id: &TaskId,
    ) -> Result<Task, Fault> {
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
        stored_task(row)
    }

    /// [`Self::edit_task`], inside the caller's transaction.
    pub(super) async fn update_task(
        &self,
        transaction: &mut SqliteConnection,
        id: &TaskId,
        read_at: Revision,
        edit: TaskEdit,
        attribution: &Attribution,
    ) -> Result<Task, Fault> {
        let task = self.read_task(transaction, id).await?;
        let edited = task.edit(read_at, edit).map_err(refused)?;
        sqlx::query(
            "UPDATE tasks SET title = ?, status = ?, order_key = ?, version = ?
             WHERE workspace_id = ? AND task_id = ?",
        )
        .bind(edited.title.as_str())
        .bind(status_text(edited.status)?)
        .bind(edited.order.as_str())
        .bind(version_number(edited.version, last_version)?)
        .bind(self.id.as_str())
        .bind(id.as_str())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        self.record_version(transaction, &edited, attribution)
            .await?;
        Ok(edited)
    }

    /// Records who is behind the version `task` is now at, and the change for the feed.
    async fn record_version(
        &self,
        transaction: &mut SqliteConnection,
        task: &Task,
        attribution: &Attribution,
    ) -> Result<(), Fault> {
        let (kind, author, agent) =
            author_columns(attribution.author()).ok_or_else(unknown_value)?;
        let version = version_number(task.version, last_version)?;
        sqlx::query(
            "INSERT INTO task_versions
                 (workspace_id, task_id, version, author_kind, author_member, agent, accepted_by,
                  at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(task.id.as_str())
        .bind(version)
        .bind(kind)
        .bind(author.as_str())
        .bind(agent)
        .bind(attribution.accepted_by().as_str())
        .bind(attribution.at())
        .execute(&mut *transaction)
        .await
        .map_err(failed)?;
        let versioned = Versioned::Task(&task.id, version);
        self.record_change(transaction, versioned, attribution.accepted_by())
            .await
    }
}

fn stored_task((id, title, status, order, version): Row) -> Result<Task, Fault> {
    Ok(Task {
        id: stored(id)?,
        title: stored(title)?,
        status: stored_status(&status)?,
        order: stored(order)?,
        version: stored_version(version)?,
    })
}

pub(crate) fn task_not_found() -> Fault {
    Fault::new(code!("task.not_found"), "There is no such task")
}

fn refused(error: EditError) -> Fault {
    match error {
        EditError::Stale => Fault::new(
            code!("task.stale"),
            "The task has changed since it was read",
        ),
        EditError::UnknownStatus => unknown_value(),
        EditError::LastVersion => last_version(),
    }
}

/// A status or an author from a newer peer, which this build can't store.
fn unknown_value() -> Fault {
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

fn status_text(status: TaskStatus) -> Result<&'static str, Fault> {
    match status {
        TaskStatus::Todo => Ok("todo"),
        TaskStatus::InProgress => Ok("in_progress"),
        TaskStatus::Done => Ok("done"),
        TaskStatus::Newer => Err(unknown_value()),
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

#[cfg(test)]
mod tests {
    use weacon_model::{OrderKey, Title};
    use weacon_protocol::{AgentKind, AgentOwner, Author};
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::stored_author;
    use crate::db::test_support::{NOW, by, code};
    use crate::test_support::{agent, member_id, new_task, two_teams};

    fn rename(title: &str) -> TaskEdit {
        TaskEdit::Title(Title::try_from(title.to_owned()).unwrap())
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

    // Fills `workspace` with `count` tasks, keyed `a11`, `a21` and on.
    async fn fill(workspace: &Workspace, count: u32) {
        sqlx::query(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?2)
             INSERT INTO tasks (workspace_id, task_id, title, status, order_key, version)
             SELECT ?1, 'f' || i, 'Filler', 'todo', 'a' || i || '1', 1 FROM n",
        )
        .bind(workspace.id.as_str())
        .bind(count)
        .execute(&workspace.db.writer)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_workspace_holds_up_to_its_most_tasks_and_no_more() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        fill(&one, MAX_TASKS - 1).await;

        one.add_task(&new_task("t1", "Last", "z"), &by("m1"))
            .await
            .unwrap();
        let full = one
            .add_task(&new_task("t2", "Over", "zz"), &by("m1"))
            .await
            .unwrap_err();

        assert_eq!(code(&full), "task.too_many");
        assert_eq!(
            one.tasks().await.unwrap().len(),
            usize::try_from(MAX_TASKS).unwrap()
        );
        two.add_task(&new_task("t2", "Elsewhere", "V"), &by("m2"))
            .await
            .unwrap();
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

    async fn authors(workspace: &Workspace) -> Vec<Author> {
        sqlx::query_as::<_, (String, String, Option<String>)>(
            "SELECT author_kind, author_member, agent FROM task_versions
             WHERE workspace_id = ? ORDER BY version",
        )
        .bind(workspace.id.as_str())
        .fetch_all(&workspace.db.readers)
        .await
        .unwrap()
        .into_iter()
        .map(|(kind, member, agent)| stored_author(&kind, member, agent.as_deref()).unwrap())
        .collect()
    }

    #[tokio::test]
    async fn a_version_an_agent_made_keeps_which_agent_when_known() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        let own = AgentOwner::Member {
            member: member_id("m1"),
        };
        let attributions = [
            Some(agent(AgentKind::Codex, own, Some("Ada's laptop"))),
            Some(agent(AgentKind::Claude, AgentOwner::Shared, None)),
            None,
        ]
        .map(|agent| Attribution::accepted(member_id("m1"), agent, member_id("m3"), NOW));
        let mut read_at = Revision::FIRST;
        for attribution in &attributions {
            let edited = one.edit_task(&task.id, read_at, rename("Ship"), attribution);
            read_at = edited.await.unwrap().version;
        }
        let someone_elses = AgentOwner::Member {
            member: member_id("m3"),
        };
        let refused = [
            agent(AgentKind::Newer, AgentOwner::Shared, None),
            agent(AgentKind::Codex, AgentOwner::Newer, None),
            agent(AgentKind::Codex, someone_elses, None),
        ]
        .map(|newer| Attribution::accepted(member_id("m1"), Some(newer), member_id("m3"), NOW));
        for attribution in &refused {
            let late = one.edit_task(&task.id, read_at, rename("Late"), attribution);
            assert_eq!(code(&late.await.unwrap_err()), "task.unknown_value");
        }

        let mut expected = vec![by("m1").author().clone()];
        expected.extend(attributions.iter().map(|made| made.author().clone()));
        assert_eq!(authors(&one).await, expected);
    }

    #[tokio::test]
    async fn every_version_records_who_made_it_and_who_accepted_it() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let task = new_task("t1", "Write", "V");
        one.add_task(&task, &by("m1")).await.unwrap();
        let agents = Attribution::accepted(member_id("m1"), None, member_id("m3"), NOW + 5);
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
        assert_eq!(author_columns(&Author::Newer), None);
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
