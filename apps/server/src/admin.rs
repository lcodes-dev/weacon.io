use std::io::Write;
use std::path::Path;

use weacon_observability::Fault;
use weacon_protocol::{AccountId, MemberId, WorkspaceId, code};

use crate::app::fresh;
use crate::cli::AdminCommand;
use crate::db::{self, Db};
use crate::email::Email;
use crate::workspaces::workspace_name;

/// Runs `command`, writing what it prints to `out`; `new_id` names what it creates.
pub(crate) async fn run(
    command: AdminCommand,
    new_id: &dyn Fn() -> String,
    out: &mut dyn Write,
) -> Result<(), Fault> {
    let printed = match command {
        AdminCommand::CreateWorkspace { db, name } => {
            let name = workspace_name(&name)?;
            let id: WorkspaceId = fresh(new_id)?;
            let db = Db::create(&db).await?;
            let created = db.create_workspace(&id, name).await;
            closed(db, created.map(|_| format!("{}\n", id.as_str()))).await
        }
        AdminCommand::AddAccount { db, email } => add_account(&db, &email, new_id).await,
        AdminCommand::AddMember {
            db,
            workspace,
            email,
        } => {
            let email = Email::parse(&email)?;
            let id: MemberId = fresh(new_id)?;
            let account: AccountId = fresh(new_id)?;
            let db = Db::open(&db).await?;
            let added = match db.workspace(&workspace).await {
                Ok(workspace) => workspace.add_member(&id, &account, &email).await,
                Err(fault) => Err(fault),
            };
            closed(db, added.map(|()| format!("{}\n", id.as_str()))).await
        }
        AdminCommand::Members { db, workspace } => {
            let db = Db::open(&db).await?;
            let members = match db.workspace(&workspace).await {
                Ok(workspace) => workspace.members().await,
                Err(fault) => Err(fault),
            };
            let lines = members.map(|members| {
                members
                    .iter()
                    .map(|member| [member.id.as_str(), " ", member.email.as_str(), "\n"].concat())
                    .collect()
            });
            closed(db, lines).await
        }
        AdminCommand::Backup { db, to } => {
            let db = Db::open(&db).await?;
            let backed_up = db.backup(&to).await;
            closed(db, backed_up.map(|()| String::new())).await
        }
        AdminCommand::Restore { from, db } => db::restore(&from, &db).await.map(|()| String::new()),
    }?;
    out.write_all(printed.as_bytes()).map_err(|error| {
        Fault::new(code!("output.failed"), "The output couldn't be written").because(error)
    })
}

async fn add_account(db: &Path, email: &str, new_id: &dyn Fn() -> String) -> Result<String, Fault> {
    let email = Email::parse(email)?;
    let id: AccountId = fresh(new_id)?;
    let db = Db::open(db).await?;
    let added = db.add_account(&id, &email).await;
    closed(db, added.map(|()| format!("{}\n", id.as_str()))).await
}

/// Closes `db`, which checkpoints its write-ahead log into the file, then gives `result`.
async fn closed(db: Db, result: Result<String, Fault>) -> Result<String, Fault> {
    db.close().await;
    result
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use weacon_testing::TempDir;

    use std::sync::{Arc, Mutex};

    use weacon_observability::Deliver;

    use super::*;
    use crate::app::App;
    use crate::test_support::{VERSION, recording_sink, workspace_id};
    use crate::workspaces::MAX_NAME;

    async fn admin(command: AdminCommand) -> Result<String, Fault> {
        let next = Cell::new(0);
        let new_id = || {
            next.set(next.get() + 1);
            format!("id{}", next.get())
        };
        let mut out = Vec::new();
        run(command, &new_id, &mut out).await?;
        Ok(String::from_utf8(out).unwrap())
    }

    #[tokio::test]
    async fn an_operator_sets_up_a_workspace() {
        let dir = TempDir::new().unwrap();
        let db = dir.join("weacon.db");
        let workspace = admin(AdminCommand::CreateWorkspace {
            db: db.clone(),
            name: " Team ".to_owned(),
        })
        .await
        .unwrap();
        let member = |email: &str| AdminCommand::AddMember {
            db: db.clone(),
            workspace: workspace_id("id1"),
            email: email.to_owned(),
        };
        let added = admin(member(" Ada@Example.com ")).await.unwrap();
        let again = admin(member("ada@example.com")).await.unwrap_err();
        let invalid = admin(member("ada")).await.unwrap_err();
        let members = admin(AdminCommand::Members {
            db: db.clone(),
            workspace: workspace_id("id1"),
        })
        .await
        .unwrap();

        assert_eq!(workspace, "id1\n");
        assert_eq!(added, "id1\n");
        assert_eq!(again.code().as_str(), "db.exists");
        assert_eq!(invalid.code().as_str(), "email.invalid");
        assert_eq!(members, "id1 ada@example.com\n");
    }

    #[tokio::test]
    async fn an_operator_makes_an_account_once_for_an_address() {
        let dir = TempDir::new().unwrap();
        let db = dir.join("weacon.db");
        Db::create(&db).await.unwrap().close().await;
        let account = |email: &str| AdminCommand::AddAccount {
            db: db.clone(),
            email: email.to_owned(),
        };

        let added = admin(account(" Ada@Example.com ")).await.unwrap();
        let again = admin(account("ada@example.com")).await.unwrap_err();
        let invalid = admin(account("ada")).await.unwrap_err();

        assert_eq!(added, "id1\n");
        assert_eq!(again.code().as_str(), "db.exists");
        assert_eq!(invalid.code().as_str(), "email.invalid");
    }

    #[tokio::test]
    async fn backups_neither_print_nor_overwrite() {
        let dir = TempDir::new().unwrap();
        let (db, backup) = (dir.join("weacon.db"), dir.join("backup.db"));
        Db::create(&db).await.unwrap();
        let to_backup = || AdminCommand::Backup {
            db: db.clone(),
            to: backup.clone(),
        };
        let printed = admin(to_backup()).await.unwrap();
        let again = admin(to_backup()).await.unwrap_err();
        let restored = admin(AdminCommand::Restore {
            from: backup.clone(),
            db: dir.join("restored.db"),
        })
        .await
        .unwrap();

        assert_eq!((printed, restored), (String::new(), String::new()));
        assert_eq!(again.code().as_str(), "db.file_exists");
    }

    #[tokio::test]
    async fn names_must_be_one_short_line() {
        let dir = TempDir::new().unwrap();
        for name in ["  ", "a\nb", &"a".repeat(MAX_NAME + 1)] {
            let error = admin(AdminCommand::CreateWorkspace {
                db: dir.join("weacon.db"),
                name: name.to_owned(),
            })
            .await
            .unwrap_err();

            assert_eq!(error.code().as_str(), "workspace.name_invalid", "{name}");
        }
        assert_eq!(
            workspace_name(&"é".repeat(MAX_NAME))
                .unwrap()
                .chars()
                .count(),
            MAX_NAME
        );
    }

    #[tokio::test]
    async fn commands_on_a_missing_database_create_nothing() {
        let dir = TempDir::new().unwrap();
        let error = admin(AdminCommand::Members {
            db: dir.join("wecon.db"),
            workspace: workspace_id("id1"),
        })
        .await
        .unwrap_err();

        assert_eq!(error.code().as_str(), "db.not_found");
        assert!(!dir.join("wecon.db").exists());
    }

    #[tokio::test]
    async fn an_unknown_workspace_is_an_error() {
        let dir = TempDir::new().unwrap();
        Db::create(&dir.join("weacon.db")).await.unwrap();
        let error = admin(AdminCommand::Members {
            db: dir.join("weacon.db"),
            workspace: workspace_id("nope"),
        })
        .await
        .unwrap_err();

        assert_eq!(error.code().as_str(), "workspace.not_found");
    }

    #[tokio::test]
    async fn commands_leave_everything_in_the_database_file() {
        let dir = TempDir::new().unwrap();
        let db = dir.join("weacon.db");
        admin(AdminCommand::CreateWorkspace {
            db: db.clone(),
            name: "Team".to_owned(),
        })
        .await
        .unwrap();
        admin(AdminCommand::AddMember {
            db: db.clone(),
            workspace: workspace_id("id1"),
            email: "ada@example.com".to_owned(),
        })
        .await
        .unwrap();

        assert!(!dir.join("weacon.db-wal").exists());
        assert!(!dir.join("weacon.db-shm").exists());
    }

    #[tokio::test]
    async fn no_content_reaches_a_log_line_or_an_error_report() {
        let dir = TempDir::new().unwrap();
        let db = dir.join("weacon.db");
        admin(AdminCommand::CreateWorkspace {
            db: db.clone(),
            name: "Team".to_owned(),
        })
        .await
        .unwrap();
        let add = || AdminCommand::AddMember {
            db: db.clone(),
            workspace: workspace_id("id1"),
            email: "marker@example.com".to_owned(),
        };
        admin(add()).await.unwrap();
        let failures = [
            admin(add()).await.unwrap_err(),
            admin(AdminCommand::CreateWorkspace {
                db: db.clone(),
                name: "MARKER\nname".to_owned(),
            })
            .await
            .unwrap_err(),
            admin(AdminCommand::Restore {
                from: dir.join("MARKER-PATH.db"),
                db: dir.join("restored.db"),
            })
            .await
            .unwrap_err(),
        ];
        let (logs, uploads) = reported(failures);

        assert_eq!(uploads.len(), 3);
        for text in std::iter::once(logs).chain(uploads) {
            assert!(!text.to_lowercase().contains("marker"), "{text}");
        }
    }

    // What logging and reporting `failures` as the server does writes: its log, and each report.
    fn reported(failures: impl IntoIterator<Item = Fault>) -> (String, Vec<String>) {
        let (stdout, records) = recording_sink();
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&uploads);
        let deliver: Deliver = Box::new(move |upload| {
            seen.lock().unwrap().push(upload.clone());
            Ok(())
        });
        let app = App::start(
            VERSION,
            stdout,
            deliver,
            Some("https://key@sentry.example.com/1"),
        );
        for fault in failures {
            app.finish(Err(fault));
        }
        let logs = serde_json::to_string(&*records.lock().unwrap()).unwrap();
        let uploads = uploads
            .lock()
            .unwrap()
            .iter()
            .map(|upload| String::from_utf8_lossy(&upload.body).into_owned())
            .collect();
        (logs, uploads)
    }
}
