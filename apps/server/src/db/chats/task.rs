//! The task a chat was made for.

use sqlx::SqliteConnection;
use weacon_observability::Fault;
use weacon_protocol::{ChatId, ChatTask};

use crate::db::{Workspace, failed, stored, unreadable};

impl Workspace {
    /// Links `chat` to `task`, inside the transaction that makes it; the database refuses a task
    /// this workspace doesn't have.
    pub(super) async fn link_task(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
        task: &ChatTask,
    ) -> Result<(), Fault> {
        sqlx::query(
            "INSERT INTO chat_tasks (workspace_id, chat_id, task_id, title) VALUES (?, ?, ?, ?)",
        )
        .bind(self.id.as_str())
        .bind(chat.as_str())
        .bind(task.id.as_str())
        .bind(&task.title)
        .execute(transaction)
        .await
        .map_err(failed)?;
        Ok(())
    }

    /// Unlinks `chat` from its task, inside the transaction that takes it back.
    pub(super) async fn unlink_task(
        &self,
        transaction: &mut SqliteConnection,
        chat: &ChatId,
    ) -> Result<(), Fault> {
        sqlx::query("DELETE FROM chat_tasks WHERE workspace_id = ? AND chat_id = ?")
            .bind(self.id.as_str())
            .bind(chat.as_str())
            .execute(transaction)
            .await
            .map_err(failed)?;
        Ok(())
    }
}

/// A chat's task as stored: its id and title, both or neither.
pub(super) fn stored_task(
    id: Option<String>,
    title: Option<String>,
) -> Result<Option<ChatTask>, Fault> {
    match (id, title) {
        (Some(id), Some(title)) => Ok(Some(ChatTask {
            id: stored(id)?,
            title,
        })),
        (None, None) => Ok(None),
        _ => Err(unreadable()),
    }
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::db::test_support::{Making, NOW, code, id, make_as};
    use crate::test_support::{member_id, new_task, two_teams};

    fn task(id: &str, title: &str) -> ChatTask {
        ChatTask {
            id: id.parse().unwrap(),
            title: title.to_owned(),
        }
    }

    fn making<'a>(chat: &'a str, owner: &'a str, task: &'a ChatTask) -> Making<'a> {
        Making {
            chat,
            owner,
            title: "Fix the build",
            machine: "mc-1",
            at: NOW,
            task: Some(task),
        }
    }

    #[tokio::test]
    async fn a_chat_keeps_its_own_workspaces_task_and_the_database_refuses_another_ones() {
        let dir = TempDir::new().unwrap();
        let (one, two) = two_teams(&dir).await;
        let by = |member| weacon_model::Attribution::by(member_id(member), NOW);
        one.add_task(&new_task("t-1", "Speed up the build", "a"), &by("m1"))
            .await
            .unwrap();
        two.add_task(&new_task("t-2", "Plan the trip", "a"), &by("m2"))
            .await
            .unwrap();
        let own = task("t-1", "Speed up the build");

        make_as(&one, &making("c-1", "m1", &own)).await.unwrap();
        let theirs = task("t-2", "Plan the trip");
        let refused = make_as(&one, &making("c-2", "m1", &theirs)).await;
        // The same id in the other workspace, which holds no such task.
        let missing = make_as(&two, &making("c-1", "m2", &own)).await;

        assert_eq!(code(&refused.unwrap_err()), "db.failed");
        assert_eq!(code(&missing.unwrap_err()), "db.failed");
        let chat = one.chat(&id("c-1")).await.unwrap().unwrap();
        assert_eq!(chat.task, Some(own.clone()));
        assert_eq!(one.chat(&id("c-2")).await.unwrap(), None);
        assert_eq!(two.chat(&id("c-1")).await.unwrap(), None);
        let listed = one.chats().await.unwrap();
        let tasks: Vec<_> = listed.iter().map(|chat| chat.task.clone()).collect();
        assert_eq!(tasks, [Some(id("t-1"))]);
        assert!(two.chats().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_chat_taken_back_takes_its_task_link_with_it() {
        let dir = TempDir::new().unwrap();
        let (one, _) = two_teams(&dir).await;
        let at = weacon_model::Attribution::by(member_id("m1"), NOW);
        one.add_task(&new_task("t-1", "Speed up the build", "a"), &at)
            .await
            .unwrap();
        let own = task("t-1", "Speed up the build");
        crate::db::test_support::begin_as(&one, &making("c-1", "m1", &own))
            .await
            .unwrap();

        one.forget_chat(&id("c-1")).await.unwrap();

        assert_eq!(one.chat(&id("c-1")).await.unwrap(), None);
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chat_tasks")
            .fetch_one(&one.db.readers)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn a_stored_task_has_both_its_id_and_title_or_neither() {
        let both = stored_task(Some("t-1".to_owned()), Some("Go".to_owned()));
        assert_eq!(both.unwrap(), Some(task("t-1", "Go")));
        assert_eq!(stored_task(None, None).unwrap(), None);
        let half = stored_task(Some("t-1".to_owned()), None).unwrap_err();
        assert_eq!(code(&half), "db.unreadable");
        let bad = stored_task(Some("../t".to_owned()), Some("Go".to_owned()));
        assert_eq!(code(&bad.unwrap_err()), "db.unreadable");
    }
}
