use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};

use sqlx::sqlite::{SqliteConnectOptions, SqliteConnection};
use sqlx::{ConnectOptions, Connection};
use weacon_observability::Fault;
use weacon_protocol::code;

use super::{Db, failed, file_path, not_found, path_unusable};

impl Db {
    /// Writes a consistent copy of the database to the new file `to`, and checks it.
    pub(crate) async fn backup(&self, to: &Path) -> Result<(), Fault> {
        let to = file_path(to)?;
        if to.exists() {
            return Err(exists());
        }
        sqlx::query("VACUUM INTO ?")
            .bind(to.to_str().ok_or_else(path_unusable)?)
            .execute(&self.writer)
            .await
            .map_err(failed)?;
        check(&to).await
    }
}

/// Checks the backup `from` and writes a copy of it to the new file `to`, which appears only
/// once the copy is checked and on disk. The server must not be running on `to`.
pub(crate) async fn restore(from: &Path, to: &Path) -> Result<(), Fault> {
    let (from, to) = (file_path(from)?, file_path(to)?);
    // SQLite would replay a journal left beside `to`, and a `.partial` may be another restore's.
    if beside(&to, &["", "-wal", "-shm", "-journal", ".partial"]) {
        return Err(exists());
    }
    // A backup made by `backup` has no log; one beside it is stale or crafted.
    if beside(&from, &["-wal", "-journal"]) {
        return Err(Fault::new(
            code!("db.backup_has_log"),
            "The backup has a log beside it; make a fresh one with backup",
        ));
    }
    check(&from).await?;
    let partial = sidecar(&to, ".partial");
    let restored = match copy(&from, &partial).await {
        Ok(()) => link(partial.clone(), to).await,
        Err(fault) => Err(fault),
    };
    drop(fs::remove_file(&partial));
    restored
}

/// Checks that the database file at `path` is whole and its references hold, without
/// changing it; any error reading it means it can't be trusted.
async fn check(path: &Path) -> Result<(), Fault> {
    if !path.exists() {
        return Err(not_found());
    }
    let mut connection = read_only(path)
        .await
        .map_err(|error| corrupt().because(error))?;
    let integrity: Vec<String> = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_all(&mut connection)
        .await
        .map_err(|error| corrupt().because(error))?;
    let broken_references = sqlx::query("PRAGMA foreign_key_check")
        .fetch_optional(&mut connection)
        .await
        .map_err(|error| corrupt().because(error))?;
    connection
        .close()
        .await
        .map_err(|error| corrupt().because(error))?;
    if integrity != ["ok"] || broken_references.is_some() {
        return Err(corrupt());
    }
    Ok(())
}

/// Copies the database `from` into the new file `to`, then checks the copy.
async fn copy(from: &Path, to: &Path) -> Result<(), Fault> {
    let mut source = read_only(from).await.map_err(restore_failed)?;
    let copied = sqlx::query("VACUUM INTO ?")
        .bind(to.to_str().ok_or_else(path_unusable)?)
        .execute(&mut source)
        .await;
    source.close().await.map_err(restore_failed)?;
    copied.map_err(restore_failed)?;
    check(to).await
}

/// Flushes `partial` and links it at `to`, which fails rather than replace a file that
/// appeared meanwhile, then makes the new name durable.
async fn link(partial: PathBuf, to: PathBuf) -> Result<(), Fault> {
    tokio::task::spawn_blocking(move || {
        OpenOptions::new().write(true).open(&partial)?.sync_all()?;
        fs::hard_link(&partial, &to)?;
        // Windows can't open a folder, and doesn't need this.
        if let Some(Ok(folder)) = to.parent().map(File::open) {
            folder.sync_all()?;
        }
        Ok(())
    })
    .await
    .map_err(io::Error::other)
    .and_then(|linked| linked)
    .map_err(restore_failed)
}

/// `path` opened read-only and immutable, so `SQLite` reads no log beside it, even one that
/// appears after `restore` looked (`RUSTSEC-2026-0459`).
async fn read_only(path: &Path) -> Result<SqliteConnection, sqlx::Error> {
    SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .immutable(true)
        .connect()
        .await
}

/// Whether any file named `path` plus one of `suffixes` exists.
fn beside(path: &Path, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| sidecar(path, suffix).exists())
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn corrupt() -> Fault {
    Fault::new(
        code!("db.corrupt"),
        "The database failed its integrity check",
    )
}

fn restore_failed(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> Fault {
    Fault::new(
        code!("db.restore_failed"),
        "The backup couldn't be restored",
    )
    .because(error)
}

fn exists() -> Fault {
    Fault::new(code!("db.file_exists"), "A file is already there")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_testing::TempDir;

    use super::*;
    use crate::email::Email;
    use crate::test_support::{member_id, workspace_id};

    async fn team(dir: &TempDir) -> Db {
        let db = Db::create(&dir.join("weacon.db")).await.unwrap();
        let team = db
            .create_workspace(&workspace_id("w1"), "Team")
            .await
            .unwrap();
        team.add_member(&member_id("m1"), &Email::parse("ada@example.com").unwrap())
            .await
            .unwrap();
        db
    }

    #[tokio::test]
    async fn a_backup_restores_to_the_same_data() {
        let dir = TempDir::new().unwrap();
        let (backup, restored) = (dir.join("backup.db"), dir.join("restored.db"));
        let db = team(&dir).await;
        db.backup(&backup).await.unwrap();
        db.workspace(&workspace_id("w1"))
            .await
            .unwrap()
            .add_member(&member_id("m2"), &Email::parse("bo@example.com").unwrap())
            .await
            .unwrap();
        restore(&backup, &restored).await.unwrap();
        let members = Db::open(&restored)
            .await
            .unwrap()
            .workspace(&workspace_id("w1"))
            .await
            .unwrap()
            .members()
            .await
            .unwrap();

        assert_eq!(members.len(), 1);
        assert_eq!(members[0].email.as_str(), "ada@example.com");
        assert!(!sidecar(&restored, ".partial").exists());
    }

    #[tokio::test]
    async fn neither_overwrites_a_file() {
        let dir = TempDir::new().unwrap();
        let db = team(&dir).await;
        let backup = dir.join("backup.db");
        db.backup(&backup).await.unwrap();

        let again = db.backup(&backup).await.unwrap_err();
        let over_live = restore(&backup, &dir.join("weacon.db")).await.unwrap_err();
        assert_eq!(again.code().as_str(), "db.file_exists");
        assert_eq!(over_live.code().as_str(), "db.file_exists");
    }

    #[tokio::test]
    async fn a_damaged_backup_is_not_restored() {
        let dir = TempDir::new().unwrap();
        let backup = dir.join("backup.db");
        team(&dir).await.backup(&backup).await.unwrap();
        let mut bytes = fs::read(&backup).unwrap();
        let page = 4096;
        for byte in bytes.iter_mut().skip(page).take(page) {
            *byte = 0xA5;
        }
        fs::write(&backup, bytes).unwrap();
        let restored = dir.join("restored.db");

        let error = restore(&backup, &restored).await.unwrap_err();
        assert_eq!(error.code().as_str(), "db.corrupt");
        assert!(!restored.exists());
    }

    #[tokio::test]
    async fn a_backup_with_broken_references_is_not_restored() {
        let dir = TempDir::new().unwrap();
        let broken = dir.join("broken.db");
        let mut connection = SqliteConnectOptions::new()
            .filename(&broken)
            .create_if_missing(true)
            .foreign_keys(false)
            .connect()
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TABLE a (id INTEGER PRIMARY KEY);
             CREATE TABLE b (a_id INTEGER REFERENCES a (id));
             INSERT INTO b VALUES (1);",
        )
        .execute(&mut connection)
        .await
        .unwrap();
        connection.close().await.unwrap();

        let error = restore(&broken, &dir.join("restored.db"))
            .await
            .unwrap_err();
        assert_eq!(error.code().as_str(), "db.corrupt");
    }

    #[tokio::test]
    async fn a_missing_backup_is_not_restored() {
        let dir = TempDir::new().unwrap();
        let restored = dir.join("restored.db");

        restore(&dir.join("missing.db"), &restored)
            .await
            .unwrap_err();
        assert!(!restored.exists());
    }

    #[tokio::test]
    async fn a_journal_left_beside_the_target_stops_a_restore() {
        let dir = TempDir::new().unwrap();
        let backup = dir.join("backup.db");
        team(&dir).await.backup(&backup).await.unwrap();
        let restored = dir.join("restored.db");
        for suffix in ["-wal", "-shm", "-journal"] {
            fs::write(sidecar(&restored, suffix), "stale").unwrap();

            let error = restore(&backup, &restored).await.unwrap_err();
            assert_eq!(error.code().as_str(), "db.file_exists", "{suffix}");
            assert!(!restored.exists());
            fs::remove_file(sidecar(&restored, suffix)).unwrap();
        }
        restore(&backup, &restored).await.unwrap();
    }

    #[tokio::test]
    async fn a_copy_that_fails_leaves_nothing_behind() {
        let dir = TempDir::new().unwrap();
        let backup = dir.join("backup.db");
        team(&dir).await.backup(&backup).await.unwrap();
        let restored = dir.join("no-folder/restored.db");

        let error = restore(&backup, &restored).await.unwrap_err();
        assert_eq!(error.code().as_str(), "db.restore_failed");
        assert!(!sidecar(&restored, ".partial").exists());
    }

    #[tokio::test]
    async fn a_backup_with_a_log_beside_it_is_not_restored() {
        let dir = TempDir::new().unwrap();
        let backup = dir.join("backup.db");
        let db = team(&dir).await;
        db.backup(&backup).await.unwrap();
        let live = dir.join("weacon.db");
        let restored = dir.join("restored.db");
        assert!(sidecar(&live, "-wal").exists());

        let from_live = restore(&live, &restored).await.unwrap_err();
        assert_eq!(from_live.code().as_str(), "db.backup_has_log");
        for suffix in ["-wal", "-journal"] {
            fs::write(sidecar(&backup, suffix), "").unwrap();
            let error = restore(&backup, &restored).await.unwrap_err();
            assert_eq!(error.code().as_str(), "db.backup_has_log", "{suffix}");
            assert!(!restored.exists());
            fs::remove_file(sidecar(&backup, suffix)).unwrap();
        }
        restore(&backup, &restored).await.unwrap();
        db.close().await;
    }

    #[tokio::test]
    async fn another_restores_partial_file_is_left_alone() {
        let dir = TempDir::new().unwrap();
        let backup = dir.join("backup.db");
        team(&dir).await.backup(&backup).await.unwrap();
        let restored = dir.join("restored.db");
        fs::write(sidecar(&restored, ".partial"), "busy").unwrap();

        let error = restore(&backup, &restored).await.unwrap_err();
        assert_eq!(error.code().as_str(), "db.file_exists");
        assert_eq!(
            fs::read_to_string(sidecar(&restored, ".partial")).unwrap(),
            "busy"
        );
    }
}
