//! What this machine keeps for a workspace, read and changed by its owner from Settings.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use weacon_agents::{Launch, SignIn};
use weacon_observability::{Fault, Logger, Reporter};
use weacon_protocol::{
    AccessLevel, AccountId, AgentKind, AgentSignIn, Asker, MachineId, MachineSettings,
    MachineSettingsAction, MachineSettingsAnswer, MachineSettingsAnswerKind,
    MachineSettingsCommand, MachineSettingsQuery, MachineSettingsQueryKind, RepoId, RequestId,
    SignInState, WorkspaceId, code,
};

use crate::git::state::Stores;
use crate::outside::Outside;
use crate::workspaces::{Settings, Workspaces, sharing, sign_ins, states, store_access};
use crate::{app, reports};

/// Who this core is, as pairing made it: the machine and its owner's account.
pub(crate) struct Identity {
    pub(crate) machine: MachineId,
    pub(crate) account: AccountId,
}

/// Where the settings live, the reporter a new answer to error reports applies to, and the
/// person's own agent folders.
#[derive(Clone)]
pub(crate) struct Host {
    pub(crate) state_dir: PathBuf,
    pub(crate) path: Option<OsString>,
    pub(crate) reporter: Arc<Reporter>,
    pub(crate) outside: Outside,
    pub(crate) log: Logger,
}

/// Answers `query` with the settings as they are, for this machine's owner only; a member of
/// a workspace the machine is shared with is told its repositories and signed-in agents.
pub(crate) async fn query(
    host: &Host,
    owner: &Identity,
    query: MachineSettingsQuery,
) -> Result<MachineSettingsAnswer, Fault> {
    let for_members = matches!(
        query.query,
        MachineSettingsQueryKind::Repos | MachineSettingsQueryKind::Agents
    );
    let shared = for_members && sharing::shares(&host.state_dir, &query.workspace).await;
    allowed(owner, &query.machine, query.by.as_ref(), shared)?;
    match query.query {
        MachineSettingsQueryKind::Current => {}
        MachineSettingsQueryKind::Repos => return repos(host, query).await,
        MachineSettingsQueryKind::Agents => {
            let all = answer(host, query.request, query.machine, query.workspace);
            return all.await.map(signed_in);
        }
        MachineSettingsQueryKind::Newer => return Err(unknown()),
    }
    answer(host, query.request, query.machine, query.workspace).await
}

// Of the settings in `answer`, only which agents are signed in.
fn signed_in(mut answer: MachineSettingsAnswer) -> MachineSettingsAnswer {
    if let MachineSettingsAnswerKind::Current(settings) = answer.answer {
        let signed_in = |sign_in: &AgentSignIn| sign_in.state == SignInState::SignedIn;
        let agents = settings.sign_ins.iter().filter(|one| signed_in(one));
        answer.answer = MachineSettingsAnswerKind::Agents {
            agents: agents.map(|sign_in| sign_in.agent).collect(),
        };
    }
    answer
}

// The ids of the repositories the owner registered for the workspace, sorted; never where they
// are on the machine.
async fn repos(host: &Host, query: MachineSettingsQuery) -> Result<MachineSettingsAnswer, Fault> {
    let workspaces = Workspaces::new(&host.state_dir, host.path.clone());
    let workspace = query.workspace.clone();
    let listed = app::blocking(move || Ok(Stores::new(&workspaces, &workspace).repos().list()))
        .await
        .map_err(|error| unknown().because(error))??;
    let mut repos: Vec<RepoId> = listed.into_iter().map(|repo| repo.id).collect();
    repos.sort_by(|one, other| one.as_str().cmp(other.as_str()));
    Ok(MachineSettingsAnswer {
        request: query.request,
        machine: query.machine,
        workspace: query.workspace,
        answer: MachineSettingsAnswerKind::Repos { repos },
    })
}

/// Changes what `command` says, for this machine's owner only, and answers with the settings
/// after the change.
pub(crate) async fn command(
    host: &Host,
    owner: &Identity,
    command: MachineSettingsCommand,
) -> Result<MachineSettingsAnswer, Fault> {
    allowed(owner, &command.machine, command.by.as_ref(), false)?;
    let changing = host.clone();
    let workspace = command.workspace.clone();
    let action = command.action;
    blocking(move || change(&changing, &workspace, &action)).await?;
    answer(host, command.request, command.machine, command.workspace).await
}

// Whether `by` may ask: the machine's owner, or anyone the server names when `shared`.
fn allowed(
    owner: &Identity,
    machine: &MachineId,
    by: Option<&Asker>,
    shared: bool,
) -> Result<(), Fault> {
    if *machine != owner.machine {
        return Err(Fault::new(
            code!("settings.other_machine"),
            "The request is for another machine",
        ));
    }
    if by.is_none_or(|by| by.account != owner.account && !shared) {
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
            // Settings the answer couldn't then show stop the change before it is made.
            let workspaces = Workspaces::new(&host.state_dir, None);
            Settings::read(&workspaces.settings_file(workspace))?;
            reports::store(&host.state_dir, *send, &host.reporter, &host.log)
        }
        MachineSettingsAction::SetAccess { level } if *level != AccessLevel::Newer => {
            let workspaces = Workspaces::new(&host.state_dir, None);
            store_access(&workspaces, workspace, *level, &host.log).map(drop)
        }
        MachineSettingsAction::SetShared { shared } => {
            let workspaces = Workspaces::new(&host.state_dir, None);
            sharing::store(&workspaces, workspace, *shared, &host.log).map(drop)
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
    let (reports, settings, places, known) = blocking(move || {
        let workspaces = Workspaces::new(&reading.state_dir, reading.path.clone());
        let reports = reports::current(&reading.state_dir);
        let settings = Settings::read(&workspaces.settings_file(&asked))?;
        let known = workspaces.root(&asked).is_dir();
        Ok((reports, settings, workspaces.places(&asked), known))
    })
    .await?;
    let (agents, launches): (Vec<AgentKind>, Vec<Option<Launch>>) = places.into_iter().unzip();
    let asked: Vec<Launch> = launches.iter().flatten().cloned().collect();
    // A workspace this machine has no folder for has no sign-in, and no vendor is run to make one.
    let answers = if known {
        sign_ins(&host.log, &asked).await
    } else {
        vec![Some(SignIn::SignedOut); asked.len()]
    };
    let states = states(&launches, answers);
    let sign_ins = agents
        .into_iter()
        .zip(states)
        .map(|(agent, state)| AgentSignIn { agent, state })
        .collect();
    Ok(MachineSettingsAnswer {
        request,
        machine,
        workspace,
        answer: MachineSettingsAnswerKind::Current(MachineSettings {
            reports,
            access: settings.access,
            shared: settings.shared,
            sign_ins,
        }),
    })
}

// The settings are small files, but a slow disk still mustn't hold up the runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Fault> + Send + 'static,
) -> Result<T, Fault> {
    app::blocking(move || Ok(work()))
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

    use weacon_protocol::{MemberId, SignInState};
    use weacon_testing::TempDir;

    use super::*;
    use crate::{platform, test_support};

    fn id<T: TryFrom<String>>(id: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(id.to_owned()).unwrap()
    }

    fn owner() -> Identity {
        Identity {
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
            reporter: Arc::clone(&app.reporter),
            outside: Outside::default(),
            log: app.log.clone(),
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
                shared: false,
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
        assert_eq!(reports::current(&dir), None);
        assert!(!host.reporter.agreed());
    }

    #[tokio::test]
    async fn the_owner_sets_reports_at_once_and_a_workspaces_access() {
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
        let kept = current(
            set(MachineSettingsAction::SetAccess {
                level: AccessLevel::ReadOnly,
            })
            .await
            .unwrap(),
        );
        let off = set(MachineSettingsAction::SetReports { send: false })
            .await
            .unwrap();

        assert_eq!(
            (kept.access, kept.reports),
            (AccessLevel::ReadOnly, Some(true))
        );
        assert_eq!(off.request.as_str(), "r-2");
        assert_eq!(current(off).reports, Some(false));
        assert!(!host.reporter.agreed());
        assert_eq!(reports::current(&dir), Some(false));
    }

    #[tokio::test]
    async fn the_owner_shares_the_machine_and_only_then_a_member_reads_its_repos_and_agents() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        let owner = owner();
        let share = |shared| {
            let action = MachineSettingsAction::SetShared { shared };
            command(&host, &owner, told(action, "mc-1", Some(by("a-1"))))
        };
        let member = |kind| query(&host, &owner, asked(kind, "mc-1", Some(by("a-2"))));
        let kinds = || {
            [
                MachineSettingsQueryKind::Repos,
                MachineSettingsQueryKind::Agents,
                MachineSettingsQueryKind::Current,
            ]
        };
        let code = |result: Result<MachineSettingsAnswer, Fault>| {
            result.unwrap_err().code().as_str().to_owned()
        };

        for kind in kinds() {
            assert_eq!(code(member(kind).await), "settings.not_owner");
        }
        let by_member = MachineSettingsAction::SetShared { shared: true };
        let forged = command(&host, &owner, told(by_member, "mc-1", Some(by("a-2")))).await;
        assert_eq!(code(forged), "settings.not_owner");
        assert!(current(share(true).await.unwrap()).shared);

        let [repos, agents, all] = kinds().map(member);
        assert_eq!(
            repos.await.unwrap().answer,
            MachineSettingsAnswerKind::Repos { repos: Vec::new() }
        );
        // No agent is installed here, so none is signed in.
        assert_eq!(
            agents.await.unwrap().answer,
            MachineSettingsAnswerKind::Agents { agents: Vec::new() }
        );
        assert_eq!(code(all.await), "settings.not_owner");
        // One with no asker named is no member's.
        let unnamed = query(
            &host,
            &owner,
            asked(MachineSettingsQueryKind::Repos, "mc-1", None),
        )
        .await;
        assert_eq!(code(unnamed), "settings.not_owner");
        assert!(!current(share(false).await.unwrap()).shared);
        assert_eq!(
            code(member(MachineSettingsQueryKind::Repos).await),
            "settings.not_owner"
        );
    }

    #[test]
    fn of_a_machines_sign_ins_members_are_told_only_the_agents_signed_in() {
        let sign_in = |agent, state| AgentSignIn { agent, state };
        let answer = MachineSettingsAnswer {
            request: id("r-1"),
            machine: id("mc-1"),
            workspace: id("w1"),
            answer: MachineSettingsAnswerKind::Current(MachineSettings {
                reports: Some(true),
                access: AccessLevel::Full,
                shared: true,
                sign_ins: vec![
                    sign_in(AgentKind::Claude, SignInState::SignedOut),
                    sign_in(AgentKind::Codex, SignInState::SignedIn),
                ],
            }),
        };

        let told = signed_in(answer).answer;

        let agents = vec![AgentKind::Codex];
        assert_eq!(told, MachineSettingsAnswerKind::Agents { agents });
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
        };
        let level = command(&host, &owner, told(level, "mc-1", Some(by("a-1")))).await;

        for result in [read, newer, level] {
            assert_eq!(result.unwrap_err().code().as_str(), "settings.unknown");
        }
        assert!(!dir.join("settings").exists());
    }

    #[tokio::test]
    async fn the_repositories_a_workspace_has_are_its_ids_alone_sorted() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        let owner = owner();
        let workspaces = Workspaces::new(&host.state_dir, None);
        let git = crate::git::Git::open(&dir, std::env::var_os("PATH").as_deref()).unwrap();
        for id in ["web", "api"] {
            let folder = dir.join("repos").join(id);
            fs::create_dir_all(&folder).unwrap();
            let init = std::process::Command::new("git")
                .args(["init", "--quiet"])
                .arg(&folder)
                .status()
                .unwrap();
            assert!(init.success());
            let repo = RepoId::try_from(id.to_owned()).unwrap();
            Stores::new(&workspaces, &"w1".parse().unwrap())
                .repos()
                .add(&git, repo, &folder)
                .unwrap();
        }
        let ask = |workspace: &str| {
            let mut ask = asked(MachineSettingsQueryKind::Repos, "mc-1", Some(by("a-1")));
            ask.workspace = workspace.parse().unwrap();
            ask
        };

        let ours = query(&host, &owner, ask("w1")).await.unwrap();
        let theirs = query(&host, &owner, ask("w2")).await.unwrap();

        let ids = ["api", "web"].map(|id| RepoId::try_from(id.to_owned()).unwrap());
        assert_eq!(
            ours.answer,
            MachineSettingsAnswerKind::Repos {
                repos: ids.to_vec()
            }
        );
        assert_eq!(
            theirs.answer,
            MachineSettingsAnswerKind::Repos { repos: Vec::new() }
        );
    }

    fn set_reports() -> MachineSettingsCommand {
        told(
            MachineSettingsAction::SetReports { send: true },
            "mc-1",
            Some(by("a-1")),
        )
    }

    #[tokio::test]
    async fn settings_that_cant_be_read_are_an_error_and_stop_a_change_first() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        fs::create_dir_all(dir.join("settings")).unwrap();
        fs::write(dir.join("settings/w-w1.json"), "not json").unwrap();
        let asking = asked(MachineSettingsQueryKind::Current, "mc-1", Some(by("a-1")));

        let read = query(&host, &owner(), asking).await;
        let changed = command(&host, &owner(), set_reports()).await;

        for result in [read, changed] {
            let code = result.unwrap_err().code().as_str().to_owned();
            assert_eq!(code, "workspace.settings_unreadable");
        }
        assert_eq!(reports::current(&dir), None);
        assert!(!host.reporter.agreed());
    }

    #[tokio::test]
    async fn an_answer_that_cant_be_saved_is_an_error() {
        let dir = TempDir::new().unwrap();
        let host = host(&dir, None);
        fs::create_dir_all(dir.join("reports.json")).unwrap();

        let unsaved = command(&host, &owner(), set_reports()).await;

        assert_eq!(unsaved.unwrap_err().code().as_str(), "reports.not_saved");
        assert!(!host.reporter.agreed());
    }

    #[tokio::test]
    async fn a_workspace_this_machine_has_no_folder_for_is_signed_out_and_runs_nothing() {
        let dir = TempDir::new().unwrap();
        for name in ["claude", "codex"] {
            platform::plant_program(&dir.join("bin"), name, 0o755);
        }
        let host = host(&dir, Some(dir.join("bin").into_os_string()));
        let asking = asked(MachineSettingsQueryKind::Current, "mc-1", Some(by("a-1")));

        let settings = current(query(&host, &owner(), asking).await.unwrap());

        let states: Vec<_> = settings
            .sign_ins
            .iter()
            .map(|sign_in| sign_in.state)
            .collect();
        assert_eq!(states, [SignInState::SignedOut, SignInState::SignedOut]);
        assert!(!dir.join("workspaces").exists());
    }

    #[tokio::test]
    async fn an_installed_agent_is_asked_and_one_that_cant_say_is_unknown() {
        let dir = TempDir::new().unwrap();
        // Empty programs can't be run, so their status check fails.
        for name in ["claude", "codex"] {
            platform::plant_program(&dir.join("bin"), name, 0o755);
        }
        fs::create_dir_all(dir.join("workspaces/w-w1")).unwrap();
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
}
