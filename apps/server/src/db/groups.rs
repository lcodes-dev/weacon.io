use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::types::Json;

use super::new_id;

#[derive(Serialize, Deserialize)]
pub struct GroupMember {
    pub id: String,
    pub name: String,
}

/// Some members of a team and what they write to each other.
#[derive(Serialize)]
pub struct Group {
    pub id: String,
    /// `None` for one called by its other members' names.
    pub name: Option<String>,
    pub project_id: Option<String>,
    pub project: Option<String>,
    pub members: Json<Vec<GroupMember>>,
    /// Whether it has a message the person hasn't read.
    pub unread: bool,
}

/// The groups of a team a person is in, the one written in last first.
pub async fn list(pool: &SqlitePool, team_id: &str, user_id: &str) -> sqlx::Result<Vec<Group>> {
    sqlx::query_as!(
        Group,
        r#"SELECT id AS "id!", name, project_id, project, members AS "members!: Json<Vec<GroupMember>>",
                  (last IS NOT NULL AND last IS NOT read) AS "unread!: bool"
           FROM (SELECT g.id, g.name, g.project_id, p.name AS project, mine.read,
                        (SELECT MAX(m.id) FROM group_messages m WHERE m.group_id = g.id) AS last,
                        (SELECT json_group_array(json_object('id', id, 'name', name)) FROM (
                             SELECT u.id, u.name FROM group_members m JOIN users u ON u.id = m.user_id
                             WHERE m.group_id = g.id ORDER BY u.name)) AS members
                 FROM group_members mine JOIN groups g ON g.id = mine.group_id
                      LEFT JOIN projects p ON p.id = g.project_id
                 WHERE mine.user_id = ? AND g.team_id = ?)
           ORDER BY COALESCE(last, id) DESC"#,
        user_id,
        team_id
    )
    .fetch_all(pool)
    .await
}

pub async fn create(
    pool: &SqlitePool,
    team_id: &str,
    name: Option<&str>,
    project_id: Option<&str>,
    members: &[String],
) -> sqlx::Result<String> {
    let mut tx = pool.begin().await?;
    let id = new_id();
    sqlx::query!(
        "INSERT INTO groups (id, team_id, name, project_id, created_at) VALUES (?, ?, ?, ?, unixepoch())",
        id,
        team_id,
        name,
        project_id
    )
    .execute(&mut *tx)
    .await?;
    for member in members {
        sqlx::query!(
            "INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?, ?)",
            id,
            member
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(id)
}

/// Names a group, and gives it a project if it has none.
pub async fn change(
    pool: &SqlitePool,
    id: &str,
    name: Option<&str>,
    project_id: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE groups SET name = ?, project_id = COALESCE(project_id, ?) WHERE id = ?",
        name,
        project_id,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn join(pool: &SqlitePool, id: &str, user_id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "INSERT OR IGNORE INTO group_members (group_id, user_id) VALUES (?, ?)",
        id,
        user_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Takes a person out of one group, or out of all of a team's when `id` is `None`. A group
/// nobody is in any more is deleted with its messages.
pub async fn leave(
    pool: &SqlitePool,
    team_id: &str,
    id: Option<&str>,
    user_id: &str,
) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query!(
        "DELETE FROM group_members WHERE user_id = ?1 AND group_id IN (
             SELECT id FROM groups WHERE team_id = ?2 AND (?3 IS NULL OR id = ?3))",
        user_id,
        team_id,
        id
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "DELETE FROM groups WHERE team_id = ?
         AND NOT EXISTS (SELECT 1 FROM group_members m WHERE m.group_id = groups.id)",
        team_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await
}

#[derive(Serialize)]
pub struct GroupMessage {
    pub id: String,
    /// Who wrote it; `None` once that person's account is gone.
    pub user_id: Option<String>,
    pub user: Option<String>,
    pub text: String,
    pub created_at: i64,
    /// For an agent's answer: which agent, for the person above, and the chat and machine it worked in.
    pub agent: Option<String>,
    pub chat_id: Option<String>,
    pub machine: Option<String>,
}

/// A group's messages after the one with id `after`, oldest first.
pub async fn messages(pool: &SqlitePool, id: &str, after: &str) -> sqlx::Result<Vec<GroupMessage>> {
    sqlx::query_as!(
        GroupMessage,
        r#"SELECT m.id, m.user_id, u.name AS "user?", m.text, m.created_at, m.agent, m.chat_id,
                  n.name AS "machine?"
           FROM group_messages m LEFT JOIN users u ON u.id = m.user_id
                LEFT JOIN chats c ON c.id = m.chat_id LEFT JOIN machines n ON n.id = c.machine_id
           WHERE m.group_id = ? AND m.id > ? ORDER BY m.id"#,
        id,
        after
    )
    .fetch_all(pool)
    .await
}

/// Posts what the team's own agent answered `user_id`.
pub async fn post_as_weacon(
    pool: &SqlitePool,
    group_id: &str,
    user_id: &str,
    text: &str,
) -> sqlx::Result<()> {
    let message = new_id();
    sqlx::query!(
        "INSERT INTO group_messages (id, group_id, user_id, agent, text, created_at)
         VALUES (?, ?, ?, 'weacon', ?, unixepoch())",
        message,
        group_id,
        user_id,
        text
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Notes that a member has read a group up to its newest message.
pub async fn read(pool: &SqlitePool, id: &str, user_id: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE group_members SET read = (SELECT MAX(id) FROM group_messages WHERE group_id = ?1)
         WHERE group_id = ?1 AND user_id = ?2",
        id,
        user_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Adds a member's message to a group, which that member has then read, and gives its id.
pub async fn write(pool: &SqlitePool, id: &str, user_id: &str, text: &str) -> sqlx::Result<String> {
    let mut tx = pool.begin().await?;
    let message = new_id();
    sqlx::query!(
        "INSERT INTO group_messages (id, group_id, user_id, text, created_at) VALUES (?, ?, ?, ?, unixepoch())",
        message,
        id,
        user_id,
        text
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE group_members SET read = ? WHERE group_id = ? AND user_id = ?",
        message,
        id,
        user_id
    )
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(message)
}

/// A person's chat with an agent in a group, on a machine that is still theirs.
pub struct GroupChat {
    pub id: String,
    pub machine_id: String,
    /// The newest message of the group its agent was told.
    pub told: Option<String>,
}

pub async fn chat(
    pool: &SqlitePool,
    group_id: &str,
    user_id: &str,
    agent: &str,
) -> sqlx::Result<Option<GroupChat>> {
    sqlx::query_as!(
        GroupChat,
        r#"SELECT id, machine_id AS "machine_id!", told FROM chats
           WHERE group_id = ? AND user_id = ? AND agent = ? AND machine_id IS NOT NULL
           ORDER BY id DESC LIMIT 1"#,
        group_id,
        user_id,
        agent
    )
    .fetch_optional(pool)
    .await
}

/// Whose agents are working in a group now, each as the person and the agent.
pub async fn working(pool: &SqlitePool, group_id: &str) -> sqlx::Result<Vec<(String, String)>> {
    let working = sqlx::query!(
        "SELECT u.name, c.agent FROM chats c JOIN users u ON u.id = c.user_id
         WHERE c.group_id = ? AND c.running = 1 ORDER BY c.id",
        group_id
    )
    .fetch_all(pool)
    .await?;
    Ok(working
        .into_iter()
        .map(|chat| (chat.name, chat.agent))
        .collect())
}

pub async fn people(pool: &SqlitePool, group_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        "SELECT user_id FROM group_members WHERE group_id = ?",
        group_id
    )
    .fetch_all(pool)
    .await
}
