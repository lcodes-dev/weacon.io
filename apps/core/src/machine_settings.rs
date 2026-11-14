//! What this machine keeps for a workspace, read and changed by its owner from Settings.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use weacon_agents::{Launch, SignIn};
use weacon_observability::{Fault, Logger, Reporter};
use weacon_protocol::{
    AccessLevel, AccountId, AgentSignIn, Asker, MachineId, MachineSettings, MachineSettingsAction,
    MachineSettingsAnswer, MachineSettingsAnswerKind, MachineSettingsCommand, MachineSettingsQuery,
    MachineSettingsQueryKind, RequestId, SignInState, WorkspaceId, code,
};

use crate::reports;
use crate::workspaces::{Settings, Workspaces, sign_ins, store_access};

/// Who this core is, as pairing made it: the machine and its owner's account.
pub(crate) struct Owner {
    pub(crate) machine: MachineId,
    pub(crate) account: AccountId,
}

/// Where the settings live, and the reporter a new answer to error reports applies to.
#[derive(Clone)]
pub(crate) struct Host {
    pub(crate) state_dir: PathBuf,
    pub(crate) path: Option<OsString>,
    pub(crate) reporter: Arc<Reporter>,
    pub(crate) log: Logger,
}

/// Answers `query` with the settings as they are, for this machine's owner only.
pub(crate) async fn query(
    host: &Host,
    owner: &Owner,
    query: MachineSettingsQuery,
) -> Result<MachineSettingsAnswer, Fault> {
    allowed(owner, &query.machine, query.by.as_ref())?;
    if matches!(query.query, MachineSettingsQueryKind::Newer) {
        return Err(unknown());
    }
    answer(host, query.request, query.machine, query.workspace).await
}

/// Changes what `command` says, for this machine's owner only, and answers with the settings
/// after the change.
pub(crate) async fn command(
    host: &Host,
    owner: &Owner,
    command: MachineSettingsCommand,
) -> Result<MachineSettingsAnswer, Fault> {
    allowed(owner, &command.machine, command.by.as_ref())?;
    let changing = host.clone();
    let workspace = command.workspace.clone();
    let action = command.action;
    blocking(move || change(&changing, &workspace, &action)).await?;
    answer(host, command.request, command.machine, command.workspace).await
}

// Others' words never reach the settings, and neither does a request meant for another machine.
fn allowed(owner: &Owner, machine: &MachineId, by: Option<&Asker>) -> Result<(), Fault> {
    if *machine != owner.machine {
        return Err(Fault::new(
            code!("settings.other_machine"),
            "The request is for another machine",
        ));
    }
    if by.is_none_or(|by| by.account != owner.account) {
        return Err(Fault::new(
            code!("settings.not_owner"),
            "Only this machine's owner can see or change its settings",
        ));
    }
    Ok(())
}

fn change(
    host: &Host,
    workspace: &WorkspaceId,
    action: &MachineSettingsAction,
) -> Result<(), Fault> {
    match action {
        MachineSettingsAction::SetReports { send } => {
            reports::store(&host.state_dir, *send, &host.reporter, &host.log)
        }
        MachineSettingsAction::SetAccess { level, team } if *level != AccessLevel::Newer => {
            let workspaces = Workspaces::new(&host.state_dir, None);
            store_access(&workspaces, workspace, *level, *team)?;
            host.log
                .info("access set", &[("workspace_id", workspace.as_str().into())]);
            Ok(())
        }
        MachineSettingsAction::SetAccess { .. } | MachineSettingsAction::Newer => Err(unknown()),
    }
}

async fn answer(
    host: &Host,
    request: RequestId,
    machine: MachineId,
    workspace: WorkspaceId,
) -> Result<MachineSettingsAnswer, Fault> {
    let reading = host.clone();
    let asked = workspace.clone();
    let (reports, settings, places) = blocking(move || {
        let workspaces = Workspaces::new(&reading.state_dir, reading.path.clone());
        // An answer that can't be read sends nothing; the core warned about it when it started.
        let reports = reports::read(&reading.state_dir).unwrap_or(None);
        let settings = Settings::read(&workspaces.settings_file(&asked))?;
        Ok((reports, settings, workspaces.places(&asked)))
    })
    .await?;
    let launches: Vec<Launch> = places
        .iter()
        .filter_map(|(_, launch)| launch.clone())
        .collect();
    let mut checked = sign_ins(&host.log, &launches).await.into_iter();
    let sign_ins = places
        .iter()
        .map(|(agent, launch)| AgentSignIn {
            agent: *agent,
            state: match launch {
                Some(_) => state(checked.next().flatten()),
                None => SignInState::NotInstalled,
            },
        })
        .collect();
    Ok(MachineSettingsAnswer {
        request,
        machine,
        workspace,
        answer: MachineSettingsAnswerKind::Current(MachineSettings {
            reports,
            access: settings.access,
            team: settings.team,
            sign_ins,
        }),
    })
}

const fn state(sign_in: Option<SignIn>) -> SignInState {
    match sign_in {
        Some(SignIn::SignedIn) => SignInState::SignedIn,
        Some(SignIn::SignedOut) => SignInState::SignedOut,
        None => SignInState::Unknown,
    }
}

// The settings are small files, but a slow disk still mustn't hold up the runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Fault> + Send + 'static,
) -> Result<T, Fault> {
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| {
            Err(Fault::new(
                code!("settings.failed"),
                "The settings couldn't be read or saved",
            )
            .because(error))
        })
}

fn unknown() -> Fault {
    Fault::new(
        code!("settings.unknown"),
        "This core doesn't know that request",
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::MemberId;
    use weacon_testing::TempDir;

    use super::*;
    use crate::{platform, test_support};

    fn id<T: TryFrom<String>>(id: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(id.to_owned()).unwrap()
    }

    fn owner() -> Owner {
        Owner {
            machine: id("mc-1"),
            account: id("a-1"),
        }
    }

    fn host(dir: &TempDir, path: Option<OsString>) -> Host {
        let app = test_support::app(Some(dir), None);
        app.reporter.set_agreed(false);
        Host {
            state_dir: dir.to_path_buf(),
            path,
            reporter: app.reporter,
            log: app.log,
        }
    }

    fn by(account: &str) -> Asker {
        Asker {
            account: id(account),
            member: id::<MemberId>("m-1"),
        }
    }

    fn asked(
        query: MachineSettingsQueryKind,
        machine: &str,
        by: Option<Asker>,
    ) -> MachineSettingsQuery {
        MachineSettingsQuery {
            request: id("r-1"),
            machine: id(machine),
            workspace: id("w1"),
            query,
            by,
        }
    }

    fn told(
        action: MachineSettingsAction,
        machine: &str,
        by: Option<Asker>,
    ) -> MachineSettingsCommand {
        MachineSettingsCommand {
            request: id("r-2"),
            machine: id(machine),
            workspace: id("w1"),
            action,
            by,
        }
    }

    fn current(answer: MachineSettingsAnswer) -> MachineSettings {
        let MachineSettingsAnswerKind::Current(settings) = answer.answer else {
            panic!("not the current settings");
        };
        settings
    }

    fn not_installed() -> Vec<AgentSignIn> {
        [AgentKind::Claude, AgentKind::Codex]
            .map(|agent| AgentSignIn {
                agent,
                state: SignInState::NotInstalled,
            })
            .to_vec()
    }

    use weacon_protocol::AgentKind;

    #[tokio::test]
    async fn the_owner_reads_the_settings_with_nothing_chosen_yet() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);

        let answer = query(
            &host,
            &owner(),
            asked(MachineSettingsQueryKind::Current, "mc-1", Some(by("a-1"))),
        )
        .await
        .unwrap();

        assert_eq!(
            (
                answer.request.as_str(),
                answer.machine.as_str(),
                answer.workspace.as_str()
            ),
            ("r-1", "mc-1", "w1")
        );
        assert_eq!(
            current(answer),
            MachineSettings {
                reports: None,
                access: AccessLevel::AskFirst,
                team: false,
                sign_ins: not_installed(),
            }
        );
    }

    #[tokio::test]
    async fn only_the_owner_asking_this_machine_gets_an_answer_or_a_change() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        let code = |result: Result<MachineSettingsAnswer, Fault>| {
            result.unwrap_err().code().as_str().to_owned()
        };
        let set = || MachineSettingsAction::SetReports { send: true };

        for (machine, asker, expected) in [
            ("mc-2", Some(by("a-1")), "settings.other_machine"),
            ("mc-1", None, "settings.not_owner"),
            ("mc-1", Some(by("a-2")), "settings.not_owner"),
        ] {
            let read = query(
                &host,
                &owner(),
                asked(MachineSettingsQueryKind::Current, machine, asker.clone()),
            )
            .await;
            let changed = command(&host, &owner(), told(set(), machine, asker)).await;
            assert_eq!(code(read), expected);
            assert_eq!(code(changed), expected);
        }
        assert_eq!(reports::read(&dir).unwrap(), None);
        assert!(!host.reporter.agreed());
    }

    #[tokio::test]
    async fn the_owner_sets_reports_at_once_and_a_workspaces_access_keeping_its_team_mark() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        let owner = owner();
        let set = |action| command(&host, &owner, told(action, "mc-1", Some(by("a-1"))));

        let reported = current(
            set(MachineSettingsAction::SetReports { send: true })
                .await
                .unwrap(),
        );
        assert_eq!(reported.reports, Some(true));
        assert!(host.reporter.agreed());
        let team = current(
            set(MachineSettingsAction::SetAccess {
                level: AccessLevel::Full,
                team: Some(true),
            })
            .await
            .unwrap(),
        );
        let kept = current(
            set(MachineSettingsAction::SetAccess {
                level: AccessLevel::ReadOnly,
                team: None,
            })
            .await
            .unwrap(),
        );
        let off = set(MachineSettingsAction::SetReports { send: false })
            .await
            .unwrap();

        assert_eq!((team.access, team.team), (AccessLevel::Full, true));
        assert_eq!(
            (kept.access, kept.team, kept.reports),
            (AccessLevel::ReadOnly, true, Some(true))
        );
        assert_eq!(off.request.as_str(), "r-2");
        assert_eq!(current(off).reports, Some(false));
        assert!(!host.reporter.agreed());
        assert_eq!(reports::read(&dir).unwrap(), Some(false));
    }

    #[tokio::test]
    async fn what_this_core_doesnt_know_changes_nothing() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        let owner = owner();
        let read = query(
            &host,
            &owner,
            asked(MachineSettingsQueryKind::Newer, "mc-1", Some(by("a-1"))),
        )
        .await;
        let newer = command(
            &host,
            &owner,
            told(MachineSettingsAction::Newer, "mc-1", Some(by("a-1"))),
        )
        .await;
        let level = MachineSettingsAction::SetAccess {
            level: AccessLevel::Newer,
            team: Some(true),
        };
        let level = command(&host, &owner, told(level, "mc-1", Some(by("a-1")))).await;

        for result in [read, newer, level] {
            assert_eq!(result.unwrap_err().code().as_str(), "settings.unknown");
        }
        assert!(!dir.join("settings").exists());
    }

    #[tokio::test]
    async fn settings_that_cant_be_read_or_saved_are_errors() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        fs::create_dir_all(dir.join("settings")).unwrap();
        fs::write(dir.join("settings/w-w1.json"), "not json").unwrap();
        let read = query(
            &host,
            &owner(),
            asked(MachineSettingsQueryKind::Current, "mc-1", Some(by("a-1"))),
        )
        .await;
        fs::create_dir_all(dir.join("reports.json")).unwrap();
        let unsaved = command(
            &host,
            &owner(),
            told(
                MachineSettingsAction::SetReports { send: true },
                "mc-1",
                Some(by("a-1")),
            ),
        )
        .await;

        assert_eq!(
            read.unwrap_err().code().as_str(),
            "workspace.settings_unreadable"
        );
        assert_eq!(unsaved.unwrap_err().code().as_str(), "reports.not_saved");
        assert!(!host.reporter.agreed());
    }

    #[tokio::test]
    async fn an_installed_agent_is_asked_and_one_that_cant_say_is_unknown() {
        let dir = TempDir::new().unwrap();
        // Empty programs can't be run, so their status check fails.
        for name in ["claude", "codex"] {
            platform::plant_program(&dir.join("bin"), name, 0o755);
        }
        let host = host(&dir, Some(dir.join("bin").into_os_string()));

        let settings = current(
            query(
                &host,
                &owner(),
                asked(MachineSettingsQueryKind::Current, "mc-1", Some(by("a-1"))),
            )
            .await
            .unwrap(),
        );

        let states: Vec<_> = settings
            .sign_ins
            .iter()
            .map(|sign_in| sign_in.state)
            .collect();
        assert_eq!(states, [SignInState::Unknown, SignInState::Unknown]);
    }

    #[test]
    fn sign_ins_travel_as_their_state() {
        assert_eq!(state(Some(SignIn::SignedIn)), SignInState::SignedIn);
        assert_eq!(state(Some(SignIn::SignedOut)), SignInState::SignedOut);
        assert_eq!(state(None), SignInState::Unknown);
    }
}
