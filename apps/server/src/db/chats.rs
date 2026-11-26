use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use sqlx::types::Json;

use super::new_id;

#[derive(Serialize)]
pub struct Chat {
    pub id: String,
    pub title: String,
    pub agent: String,
    pub branch: String,
    pub running: bool,
    pub project_id: String,
    pub project: String,
    pub user_id: String,
    pub user: String,
    /// The machine it runs on, unless that was removed.
    pub machine_id: Option<String>,
    pub machine: Option<String>,
    /// The pull request it opened, once it has.
    pub pull_number: Option<i64>,
    pub pull_url: Option<String>,
    /// The hosts its commands may reach.
    pub hosts: Json<Vec<String>>,
    /// The task it is for, if any, and what that is called, as `SF-12 Returns link`.
    pub task_id: Option<String>,
    pub task: Option<String>,
    /// The group its agent answers in, when its owner called it there.
    pub group_id: Option<String>,
}

/// The chats of a team that a member may read, newest first, or only the one with `id`. A
/// chat of a group is read by that group's members alone.
pub async fn list(
    pool: &SqlitePool,
    team_id: &str,
    user_id: &str,
    id: Option<&str>,
) -> sqlx::Result<Vec<Chat>> {
    sqlx::query_as!(
        Chat,
        r#"SELECT c.id, c.title, c.agent, c.branch, c.running AS "running: bool", c.project_id,
                  p.name AS project, c.user_id, u.name AS user, c.machine_id, m.name AS "machine?",
                  c.pull_number, c.pull_url, c.hosts AS "hosts: Json<Vec<String>>", c.task_id,
                  p.key || '-' || t.number || ' ' || t.title AS "task?: String", c.group_id
           FROM projects p JOIN chats c ON c.project_id = p.id JOIN users u ON u.id = c.user_id
           LEFT JOIN machines m ON m.id = c.machine_id LEFT JOIN tasks t ON t.id = c.task_id
           WHERE p.team_id = ?1 AND (?2 IS NULL OR c.id = ?2)
             AND (c.group_id IS NULL OR EXISTS (
                 SELECT 1 FROM group_members g WHERE g.group_id = c.group_id AND g.user_id = ?3))
           ORDER BY c.id DESC"#,
        team_id,
        id,
        user_id
    )
    .fetch_all(pool)
    .await
}

/// A chat to start: whose it is, where its agent works, and what it is asked first.
pub struct NewChat<'a> {
    pub project_id: &'a str,
    pub user_id: &'a str,
    pub machine_id: &'a str,
    pub agent: &'a str,
    pub branch: &'a str,
    pub asked: &'a str,
    pub task_id: Option<&'a str>,
    /// The group its agent answers in, and the newest message there it is told.
    pub group: Option<(&'a str, &'a str)>,
}

/// What the person wrote is the chat's first event, and its first turn is running.
pub async fn start(pool: &SqlitePool, chat: &NewChat<'_>) -> sqlx::Result<String> {
    let mut tx = pool.begin().await?;
    let id = new_id();
    let asked = chat.asked.trim();
    let (group_id, told) = chat.group.unzip();
    // A chat is called by how it began.
    let title = asked.lines().next().unwrap_or_default();
    let title: String = title.chars().take(60).collect();
    sqlx::query!(
        "INSERT INTO chats (id, project_id, user_id, machine_id, agent, branch, title, asked, task_id, group_id, told, tools_token, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, lower(hex(randomblob(32))), unixepoch())",
        id,
        chat.project_id,
        chat.user_id,
        chat.machine_id,
        chat.agent,
        chat.branch,
        title,
        asked,
        chat.task_id,
        group_id,
        told
    )
    .execute(&mut *tx)
    .await?;
    add_event(
        &mut tx,
        &id,
        &serde_json::json!({"kind": "asked", "text": asked}),
    )
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn add_event(
    tx: &mut sqlx::SqliteConnection,
    chat_id: &str,
    body: &serde_json::Value,
) -> sqlx::Result<()> {
    let (id, body) = (new_id(), body.to_string());
    sqlx::query!(
        "INSERT INTO chat_events (id, chat_id, body) VALUES (?, ?, ?)",
        id,
        chat_id,
        body
    )
    .execute(tx)
    .await?;
    Ok(())
}

/// Starts a chat's next turn with what its owner wrote. False while a turn is running.
/// `told` is the newest message of its group that this tells its agent, for a group's chat.
pub async fn ask(
    pool: &SqlitePool,
    id: &str,
    asked: &str,
    told: Option<&str>,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let started = sqlx::query!(
        "UPDATE chats SET running = 1, asked = ?, told = COALESCE(?, told),
                tools_token = lower(hex(randomblob(32)))
         WHERE id = ? AND running = 0",
        asked,
        told,
        id
    )
    .execute(&mut *tx)
    .await?;
    if started.rows_affected() == 0 {
        return Ok(false);
    }
    add_event(
        &mut tx,
        id,
        &serde_json::json!({"kind": "asked", "text": asked}),
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Takes in the `number`th event a machine tells of a chat of its own, when it is the next
/// one. A turn that ended well gives the session to go on with and the diff of what the
/// chat changed, which is kept once, on the chat, and not with every turn's end.
pub async fn hear(
    pool: &SqlitePool,
    machine_id: &str,
    id: &str,
    number: i64,
    event: &core_link::Event,
) -> sqlx::Result<()> {
    use core_link::Event;
    let (running, session, diff) = match event {
        Event::Done { session, diff, .. } => (false, Some(session), Some(diff)),
        Event::Failed { .. } => (false, None, None),
        Event::Said { .. } | Event::Did { .. } | Event::Wants { .. } => (true, None, None),
    };
    let mut body = serde_json::json!(event);
    body.as_object_mut().map(|body| body.remove("diff"));
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let next = sqlx::query!(
        "UPDATE chats SET heard = heard + 1, running = ?, session = COALESCE(?, session),
                diff = COALESCE(?, diff)
         WHERE id = ? AND machine_id = ? AND heard = ? - 1",
        running,
        session,
        diff,
        id,
        machine_id,
        number
    )
    .execute(&mut *tx)
    .await?;
    if next.rows_affected() == 1 {
        add_event(&mut tx, id, &body).await?;
        match event {
            Event::Done { .. } => answer_group(&mut tx, id, None).await?,
            Event::Failed { why } => answer_group(&mut tx, id, Some(why)).await?,
            _ => {}
        }
        tx.commit().await?;
    }
    Ok(())
}

/// Posts the end of a turn in the chat's group, if it has one: the agent's last words of the
/// turn, or why it failed.
async fn answer_group(
    tx: &mut sqlx::SqliteConnection,
    chat_id: &str,
    failed: Option<&str>,
) -> sqlx::Result<()> {
    let said = sqlx::query_scalar!(
        r#"SELECT json_extract(body, '$.text') AS "text?: String" FROM chat_events
           WHERE chat_id = ?1 AND json_extract(body, '$.kind') = 'said' AND id > (
               SELECT MAX(id) FROM chat_events
               WHERE chat_id = ?1 AND json_extract(body, '$.kind') = 'asked')
           ORDER BY id DESC LIMIT 1"#,
        chat_id
    )
    .fetch_optional(&mut *tx)
    .await?;
    let text = match failed {
        Some(why) => format!("It couldn't answer: {why}"),
        None => said
            .flatten()
            .unwrap_or("Done, with nothing to say.".into()),
    };
    let message = new_id();
    sqlx::query!(
        "INSERT INTO group_messages (id, group_id, user_id, agent, chat_id, text, created_at)
         SELECT ?, group_id, user_id, agent, id, ?, unixepoch() FROM chats
         WHERE id = ? AND group_id IS NOT NULL",
        message,
        text,
        chat_id
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Ends a chat's turn with why it failed, when the machine can't say so itself.
pub async fn fail(pool: &SqlitePool, id: &str, why: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let ended = sqlx::query!(
        "UPDATE chats SET running = 0 WHERE id = ? AND running = 1",
        id
    )
    .execute(&mut *tx)
    .await?;
    if ended.rows_affected() == 1 {
        add_event(
            &mut tx,
            id,
            &serde_json::json!({"kind": "failed", "why": why}),
        )
        .await?;
        answer_group(&mut tx, id, Some(why)).await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Puts a host among those a chat's commands may reach, and says so in the chat.
pub async fn allow_host(pool: &SqlitePool, id: &str, host: &str) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    sqlx::query!(
        "UPDATE chats SET hosts = (
             SELECT json_group_array(value)
             FROM (SELECT value FROM json_each(chats.hosts) UNION SELECT ?))
         WHERE id = ?",
        host,
        id
    )
    .execute(&mut *tx)
    .await?;
    let allowed = serde_json::json!({"kind": "allowed", "host": host});
    add_event(&mut tx, id, &allowed).await?;
    tx.commit().await
}

/// Takes a host off those a chat's commands may reach.
pub async fn forget_host(pool: &SqlitePool, id: &str, host: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE chats SET hosts = (
             SELECT json_group_array(value) FROM json_each(chats.hosts) WHERE value <> ?)
         WHERE id = ?",
        host,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Everything a chat changed, as git writes it.
pub async fn diff(pool: &SqlitePool, team_id: &str, id: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        "SELECT c.diff FROM chats c JOIN projects p ON p.id = c.project_id
         WHERE c.id = ? AND p.team_id = ?",
        id,
        team_id
    )
    .fetch_optional(pool)
    .await
}

pub async fn set_pull(pool: &SqlitePool, id: &str, number: i64, url: &str) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE chats SET pull_number = ?, pull_url = ? WHERE id = ?",
        number,
        url,
        id
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// A chat whose turn is running, with what its machine needs to run it or to go on telling of it.
pub struct Running {
    pub id: String,
    pub team_id: String,
    pub agent: String,
    pub branch: String,
    pub asked: String,
    pub session: Option<String>,
    pub heard: i64,
    pub repo: Option<String>,
    pub hosts: Json<Vec<String>>,
    pub tools_token: Option<String>,
}

/// The chats whose turn is running on a machine, or on none any more when `machine_id` is `None`.
pub async fn running(pool: &SqlitePool, machine_id: Option<&str>) -> sqlx::Result<Vec<Running>> {
    sqlx::query_as!(
        Running,
        r#"SELECT c.id, p.team_id, c.agent, c.branch, c.asked, c.session, c.heard, p.repo,
                  c.hosts AS "hosts: Json<Vec<String>>", c.tools_token
           FROM chats c JOIN projects p ON p.id = c.project_id
           WHERE c.machine_id IS ? AND c.running = 1"#,
        machine_id
    )
    .fetch_all(pool)
    .await
}

/// A chat's events after the one with id `after`, as JSON, each with its id.
pub async fn events(pool: &SqlitePool, chat_id: &str, after: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar!(
        r#"SELECT json_set(body, '$.id', id) AS "event!: String" FROM chat_events
           WHERE chat_id = ? AND id > ? ORDER BY id"#,
        chat_id,
        after
    )
    .fetch_all(pool)
    .await
}

/// Whom an agent's tools are run for.
#[derive(Clone, Serialize, Deserialize)]
pub struct Caller {
    /// The chat the agent works in, which the team's own agent has none of.
    pub chat_id: Option<String>,
    pub team_id: String,
    pub project_id: String,
    /// The project's key.
    pub key: String,
    /// Who the agent works for.
    pub user_id: String,
    pub task_id: Option<String>,
    /// Which agent: a program's name, or the team's own.
    pub agent: String,
}

/// Whom the agent of the chat with this running turn's token works for.
pub async fn caller(pool: &SqlitePool, token: &str) -> sqlx::Result<Option<Caller>> {
    sqlx::query_as!(
        Caller,
        r#"SELECT c.id AS "chat_id?", p.team_id, c.project_id, p.key, c.user_id, c.task_id, c.agent
           FROM chats c JOIN projects p ON p.id = c.project_id
           WHERE c.tools_token = ? AND c.running = 1"#,
        token
    )
    .fetch_optional(pool)
    .await
}

/// Adds to a chat something the server did in it.
pub async fn note(pool: &SqlitePool, chat_id: &str, body: &serde_json::Value) -> sqlx::Result<()> {
    add_event(&mut *pool.acquire().await?, chat_id, body).await
}

pub async fn delete(pool: &SqlitePool, id: &str) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM chats WHERE id = ?", id)
        .execute(pool)
        .await?;
    Ok(())
}

/// Who may read a chat, and whether that is its group: its group's members if it has one,
/// else its team's.
pub async fn people(pool: &SqlitePool, chat_id: &str) -> sqlx::Result<(Vec<String>, bool)> {
    let chat = sqlx::query!(
        "SELECT group_id, project_id FROM chats WHERE id = ?",
        chat_id
    )
    .fetch_optional(pool)
    .await?;
    Ok(match chat {
        Some(chat) => match chat.group_id {
            Some(group) => (super::groups::people(pool, &group).await?, true),
            None => (
                super::projects::people(pool, &chat.project_id).await?,
                false,
            ),
        },
        None => Default::default(),
    })
}
