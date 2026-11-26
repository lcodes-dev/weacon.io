use sqlx::SqlitePool;

use crate::github::GithubApp;
use crate::mail::Smtp;
use crate::secrets::ServerKey;
use crate::writer::Writer;

const SMTP_PASSWORD: &str = "config.smtp_password";
const GITHUB_PRIVATE_KEY: &str = "github_private_key";
const API_KEY: &str = "writer_key";

fn team_secret(column: &str, team_id: &str) -> String {
    format!("teams.{column} {team_id}")
}

fn open(server_key: &ServerKey, place: &str, sealed: String) -> sqlx::Result<String> {
    server_key.open(place, &sealed).ok_or_else(|| {
        sqlx::Error::Decode(format!("{place} doesn't open with the server's key").into())
    })
}

pub async fn smtp(pool: &SqlitePool, server_key: &ServerKey) -> sqlx::Result<Option<Smtp>> {
    let smtp = sqlx::query_as!(
        Smtp,
        r#"SELECT smtp_host AS "host!", smtp_port AS "port!: _", smtp_security AS "security!: _",
                  smtp_username AS username, smtp_password AS password, mail_from AS "from!"
           FROM config WHERE smtp_host IS NOT NULL"#
    )
    .fetch_optional(pool)
    .await?;
    let Some(mut smtp) = smtp else {
        return Ok(None);
    };
    smtp.password = smtp
        .password
        .map(|sealed| open(server_key, SMTP_PASSWORD, sealed))
        .transpose()?;
    Ok(Some(smtp))
}

pub async fn save_smtp(
    pool: &SqlitePool,
    server_key: &ServerKey,
    smtp: Option<&Smtp>,
) -> sqlx::Result<()> {
    let password = smtp.and_then(|smtp| smtp.password.as_ref());
    let password = password.map(|password| server_key.seal(SMTP_PASSWORD, password));
    sqlx::query!(
        "UPDATE config SET smtp_host = ?, smtp_port = ?, smtp_security = ?,
                           smtp_username = ?, smtp_password = ?, mail_from = ?",
        smtp.map(|smtp| &smtp.host),
        smtp.map(|smtp| smtp.port),
        smtp.map(|smtp| smtp.security),
        smtp.and_then(|smtp| smtp.username.as_ref()),
        password,
        smtp.map(|smtp| &smtp.from)
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn github_app(
    pool: &SqlitePool,
    server_key: &ServerKey,
    team_id: &str,
) -> sqlx::Result<Option<GithubApp>> {
    let app = sqlx::query_as!(
        GithubApp,
        r#"SELECT github_app_id AS "id!", github_app_slug AS "slug!", github_private_key AS "pem!"
           FROM teams WHERE id = ? AND github_app_id IS NOT NULL"#,
        team_id
    )
    .fetch_optional(pool)
    .await?;
    let Some(mut app) = app else {
        return Ok(None);
    };
    app.pem = open(
        server_key,
        &team_secret(GITHUB_PRIVATE_KEY, team_id),
        app.pem,
    )?;
    Ok(Some(app))
}

pub async fn save_github_app(
    pool: &SqlitePool,
    server_key: &ServerKey,
    team_id: &str,
    app: Option<&GithubApp>,
) -> sqlx::Result<()> {
    let place = team_secret(GITHUB_PRIVATE_KEY, team_id);
    let pem = app.map(|app| server_key.seal(&place, &app.pem));
    sqlx::query!(
        "UPDATE teams SET github_app_id = ?, github_app_slug = ?, github_private_key = ?
         WHERE id = ?",
        app.map(|app| app.id),
        app.map(|app| &app.slug),
        pem,
        team_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn writer(
    pool: &SqlitePool,
    server_key: &ServerKey,
    team_id: &str,
) -> sqlx::Result<Option<Writer>> {
    let writer = sqlx::query_as!(
        Writer,
        r#"SELECT writer AS "kind!: _", writer_key AS key, writer_model AS model, '' AS "asker!"
           FROM teams WHERE id = ? AND writer IS NOT NULL"#,
        team_id
    )
    .fetch_optional(pool)
    .await?;
    let Some(mut writer) = writer else {
        return Ok(None);
    };
    writer.key = writer
        .key
        .map(|sealed| open(server_key, &team_secret(API_KEY, team_id), sealed))
        .transpose()?;
    Ok(Some(writer))
}

pub async fn save_writer(
    pool: &SqlitePool,
    team_id: &str,
    writer: Option<&Writer>,
) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE teams SET writer = ?, writer_model = ? WHERE id = ?",
        writer.map(|writer| writer.kind),
        writer.and_then(|writer| writer.model.as_ref()),
        team_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn api_key(
    pool: &SqlitePool,
    server_key: &ServerKey,
    team_id: &str,
) -> sqlx::Result<Option<(String, Option<String>)>> {
    let saved = sqlx::query!(
        r#"SELECT writer_key AS "key!", CASE WHEN writer = 'claude_api' THEN writer_model END AS model
           FROM teams WHERE id = ? AND writer_key IS NOT NULL"#,
        team_id
    )
    .fetch_optional(pool)
    .await?;
    let Some(saved) = saved else {
        return Ok(None);
    };
    let key = open(server_key, &team_secret(API_KEY, team_id), saved.key)?;
    Ok(Some((key, saved.model)))
}

pub async fn save_api_key(
    pool: &SqlitePool,
    server_key: &ServerKey,
    team_id: &str,
    api_key: Option<&str>,
) -> sqlx::Result<()> {
    let place = team_secret(API_KEY, team_id);
    let sealed = api_key.map(|api_key| server_key.seal(&place, api_key));
    sqlx::query!(
        "UPDATE teams SET writer_key = ? WHERE id = ?",
        sealed,
        team_id
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn secrets_sealed(pool: &SqlitePool) -> sqlx::Result<bool> {
    let sealed = sqlx::query_scalar!("SELECT secrets_sealed FROM config")
        .fetch_one(pool)
        .await?;
    Ok(sealed != 0)
}

pub async fn secrets(pool: &SqlitePool) -> sqlx::Result<Vec<(String, String)>> {
    let password = sqlx::query_scalar!("SELECT smtp_password FROM config")
        .fetch_one(pool)
        .await?;
    let mut secrets: Vec<(String, String)> = password
        .map(|password| (SMTP_PASSWORD.to_owned(), password))
        .into_iter()
        .collect();
    let teams = sqlx::query!(
        r#"SELECT id AS "id!", github_private_key, writer_key FROM teams
           WHERE github_private_key IS NOT NULL OR writer_key IS NOT NULL"#
    )
    .fetch_all(pool)
    .await?;
    for team in teams {
        if let Some(pem) = team.github_private_key {
            secrets.push((team_secret(GITHUB_PRIVATE_KEY, &team.id), pem));
        }
        if let Some(api_key) = team.writer_key {
            secrets.push((team_secret(API_KEY, &team.id), api_key));
        }
    }
    Ok(secrets)
}

pub async fn seal_secrets(pool: &SqlitePool, server_key: &ServerKey) -> sqlx::Result<()> {
    let mut tx = pool.begin_with("BEGIN IMMEDIATE").await?;
    let config = sqlx::query!("SELECT secrets_sealed, smtp_password FROM config")
        .fetch_one(&mut *tx)
        .await?;
    if config.secrets_sealed != 0 {
        return Ok(());
    }
    let password = config
        .smtp_password
        .map(|password| server_key.seal(SMTP_PASSWORD, &password));
    sqlx::query!(
        "UPDATE config SET smtp_password = ?, secrets_sealed = 1",
        password
    )
    .execute(&mut *tx)
    .await?;
    let teams = sqlx::query!(
        r#"SELECT id AS "id!", github_private_key, writer_key FROM teams
           WHERE github_private_key IS NOT NULL OR writer_key IS NOT NULL"#
    )
    .fetch_all(&mut *tx)
    .await?;
    for team in teams {
        let seal = |column, plain: String| server_key.seal(&team_secret(column, &team.id), &plain);
        let pem = team
            .github_private_key
            .map(|pem| seal(GITHUB_PRIVATE_KEY, pem));
        let api_key = team.writer_key.map(|api_key| seal(API_KEY, api_key));
        sqlx::query!(
            "UPDATE teams SET github_private_key = ?, writer_key = ? WHERE id = ?",
            pem,
            api_key,
            team.id
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}
