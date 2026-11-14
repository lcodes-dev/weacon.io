//! What this machine keeps for each workspace: a folder of its own, holding each agent's
//! config folder and so its sign-in.

mod access;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use weacon_agents::{Launch, SignIn};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{AccessLevel, AgentKind, SignInState, WorkspaceId, code};

pub(crate) use access::{Settings, parse_level, run as access, store as store_access};

use crate::app::{App, runtime};
use crate::platform;

/// Runs the vendor's own sign-in for `agent` in this terminal, against `workspace`'s config
/// folder.
pub(crate) fn login(app: &App, agent: AgentKind, workspace: &WorkspaceId) -> Result<(), Fault> {
    let state_dir = app.state_dir()?;
    let workspaces = Workspaces::new(state_dir, app.path.clone());
    let launch = workspaces.prepared(workspace, agent, workspaces.root(workspace))?;
    runtime()?.block_on(launch.log_in())?;
    let ids = [("workspace_id", workspace.as_str().into())];
    app.log.info("login finished", &ids);
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct Workspaces {
    state: PathBuf,
    dir: PathBuf,
    settings: PathBuf,
    path: Option<OsString>,
}

impl Workspaces {
    pub(crate) fn new(state_dir: &Path, path: Option<OsString>) -> Self {
        Self {
            state: state_dir.to_path_buf(),
            dir: state_dir.join("workspaces"),
            settings: state_dir.join("settings"),
            path,
        }
    }

    // No folder is made.
    fn located(
        &self,
        workspace: &WorkspaceId,
        agent: AgentKind,
        cwd: PathBuf,
    ) -> Result<Launch, Fault> {
        let find = |name: &str| platform::find_program(name, self.path.as_deref()?);
        Launch::new(agent, find, &self.root(workspace), cwd)
    }

    /// The workspaces this machine has a folder for, sorted by id; none before the first.
    fn known(&self) -> io::Result<Vec<WorkspaceId>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut known = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Some(workspace) = entry.file_name().to_str().and_then(workspace_id)
            {
                known.push(workspace);
            }
        }
        known.sort_by(|one, other| one.as_str().cmp(other.as_str()));
        Ok(known)
    }

    /// A line for each known workspace with its access level and whether each agent is signed
    /// in, then a line for each warning. `sign_ins` answers for all the launches at once, in
    /// their order; `None` shows as unknown.
    ///
    /// # Errors
    /// When the workspaces' folder can't be listed, so the lines would leave some out.
    pub(crate) fn describe(
        &self,
        sign_ins: impl FnOnce(&[Launch]) -> Vec<Option<SignIn>>,
    ) -> io::Result<String> {
        let known = self.known()?;
        let places: Vec<Option<Launch>> = known
            .iter()
            .flat_map(|workspace| self.places(workspace).map(|(_, launch)| launch))
            .collect();
        let asked: Vec<Launch> = places.iter().flatten().cloned().collect();
        let mut states = states(&places, sign_ins(&asked)).into_iter();
        let lines = known.iter().map(|workspace| {
            let agents = AGENTS.map(|(_, name)| {
                let state = states.next().unwrap_or(SignInState::Unknown);
                format!("{name} {}", state_name(state))
            });
            let (id, agents) = (workspace.as_str(), agents.join(", "));
            match Settings::read(&self.settings_file(workspace)) {
                Ok(settings) => {
                    let (level, warning) = (settings.level(), settings.warning());
                    format!("Workspace {id}: access {level}; {agents}\n{warning}")
                }
                Err(_) => format!("Workspace {id}: access unknown; {agents}\n"),
            }
        });
        Ok(lines.collect())
    }

    /// Each agent this core runs, and how to ask it about `workspace`; none for an agent whose
    /// program isn't installed, which is the only way the lookup fails for a known agent.
    pub(crate) fn places(&self, workspace: &WorkspaceId) -> [(AgentKind, Option<Launch>); 2] {
        let root = self.root(workspace);
        AGENTS.map(|(agent, _)| (agent, self.located(workspace, agent, root.clone()).ok()))
    }

    /// The folder for everything this machine keeps for `workspace` but its settings.
    pub(crate) fn root(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir.join(folder_name(workspace))
    }

    /// The file of `workspace`'s settings, outside every folder an agent is given.
    pub(crate) fn settings_file(&self, workspace: &WorkspaceId) -> PathBuf {
        self.settings
            .join(format!("{}.json", folder_name(workspace)))
    }

    /// How to start `agent` in `cwd` with `workspace`'s own config folder; the only place runs
    /// get an agent, and none when the workspace's access level doesn't let agents run.
    pub(crate) fn launch(
        &self,
        workspace: &WorkspaceId,
        agent: AgentKind,
        cwd: PathBuf,
    ) -> Result<Launch, Fault> {
        if self.overlaps_state(&cwd) {
            return Err(Fault::new(
                code!("workspace.cwd_refused"),
                "Agents can't work in or above Weacon's own folder",
            ));
        }
        let access = Settings::read(&self.settings_file(workspace))?.access;
        if matches!(access, AccessLevel::Off | AccessLevel::Newer) {
            return Err(Fault::new(
                code!("access.off"),
                "This workspace's agents are turned off on this machine",
            ));
        }
        Ok(Launch {
            access,
            ..self.prepared(workspace, agent, cwd)?
        })
    }

    // Whether `cwd` is inside the state folder or holds it, so a run could reach the settings;
    // a folder that can't be resolved counts as one that does.
    fn overlaps_state(&self, cwd: &Path) -> bool {
        let (Some(state), Some(cwd)) = (resolved(&self.state), resolved(cwd)) else {
            return true;
        };
        cwd.starts_with(&state) || state.starts_with(&cwd)
    }

    // A launch whose config folder exists, private to this user.
    fn prepared(
        &self,
        workspace: &WorkspaceId,
        agent: AgentKind,
        cwd: PathBuf,
    ) -> Result<Launch, Fault> {
        let launch = self.located(workspace, agent, cwd)?;
        platform::private_dir(&launch.config_dir).map_err(|error| {
            Fault::new(
                code!("workspace.folder_failed"),
                "The workspace's config folder couldn't be created",
            )
            .because(error)
        })?;
        Ok(launch)
    }
}

// `path` with its deepest existing folder resolved, so a missing path compares like an existing
// one; `None` when something on the way exists but can't be resolved, such as a dangling symlink.
fn resolved(path: &Path) -> Option<PathBuf> {
    let mut missing = Vec::new();
    let mut base = path;
    loop {
        match base.canonicalize() {
            Ok(found) => {
                return Some(
                    missing
                        .iter()
                        .rev()
                        .fold(found, |path, name| path.join(name)),
                );
            }
            Err(error) if error.kind() != io::ErrorKind::NotFound => return None,
            Err(_) if base.symlink_metadata().is_ok() => return None,
            Err(_) => {}
        }
        missing.push(base.file_name()?);
        base = base.parent()?;
    }
}

// Vendor programs asked at once: enough that a slow one doesn't hold up a report, few enough
// for a small machine.
const AT_ONCE: usize = 8;

/// Each launch's sign-in, a few at once; a check that fails is logged here, once, and is `None`.
pub(crate) async fn sign_ins(log: &Logger, launches: &[Launch]) -> Vec<Option<SignIn>> {
    let failed = |err: Field| log.warn("sign-in check failed", &[("err", err)]);
    let mut answers = Vec::new();
    for together in launches.chunks(AT_ONCE) {
        let checks: Vec<_> = together
            .iter()
            .cloned()
            .map(|launch| tokio::spawn(async move { launch.sign_in().await }))
            .collect();
        for check in checks {
            answers.push(match check.await {
                Ok(Ok(sign_in)) => Some(sign_in),
                Ok(Err(error)) => {
                    failed(Field::error(&error));
                    None
                }
                Err(error) => {
                    failed(Field::error(&error));
                    None
                }
            });
        }
    }
    answers
}

const AGENTS: [(AgentKind, &str); 2] = [
    (AgentKind::Claude, "Claude Code"),
    (AgentKind::Codex, "Codex"),
];

fn folder_name(workspace: &WorkspaceId) -> String {
    platform::case_safe_name("w-", workspace.as_str())
}

/// Each place's sign-in, given `answers` for the places that have a launch, in their order.
pub(crate) fn states(places: &[Option<Launch>], answers: Vec<Option<SignIn>>) -> Vec<SignInState> {
    let mut answers = answers.into_iter();
    places
        .iter()
        .map(
            |place| match place.as_ref().map(|_| answers.next().flatten()) {
                None => SignInState::NotInstalled,
                Some(Some(SignIn::SignedIn)) => SignInState::SignedIn,
                Some(Some(SignIn::SignedOut)) => SignInState::SignedOut,
                Some(None) => SignInState::Unknown,
            },
        )
        .collect()
}

const fn state_name(state: SignInState) -> &'static str {
    match state {
        SignInState::SignedIn => "signed in",
        SignInState::SignedOut => "signed out",
        SignInState::NotInstalled => "not installed",
        SignInState::Unknown | SignInState::Newer => "unknown",
    }
}

/// The workspace that `name` is the folder of, if any.
fn workspace_id(name: &str) -> Option<WorkspaceId> {
    let mut id = String::new();
    let mut letters = name.strip_prefix("w-")?.chars();
    while let Some(letter) = letters.next() {
        id.push(if letter == '.' {
            letters.next()?.to_ascii_uppercase()
        } else {
            letter
        });
    }
    let id = WorkspaceId::try_from(id).ok()?;
    (folder_name(&id) == name).then_some(id)
}

#[cfg(test)]
mod tests {

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support;

    fn id(id: &str) -> WorkspaceId {
        WorkspaceId::try_from(id.to_owned()).unwrap()
    }

    fn program(dir: &Path, name: &str) -> PathBuf {
        platform::plant_program(&dir.join("bin"), name, 0o755)
    }

    /// Workspaces in `dir`, on a machine with both vendors' programs installed in `dir/bin`.
    fn workspaces(dir: &Path) -> Workspaces {
        for name in ["claude", "codex"] {
            program(dir, name);
        }
        Workspaces::new(dir, Some(dir.join("bin").into_os_string()))
    }

    #[test]
    fn each_workspace_and_agent_gets_its_own_config_folder() {
        let dir = TempDir::new().unwrap();
        let workspaces = workspaces(&dir);
        let launch = |workspace: &str, agent| {
            workspaces
                .launch(&id(workspace), agent, PathBuf::from("/work"))
                .unwrap()
        };

        let claude = launch("w1", AgentKind::Claude);
        let codex = launch("w1", AgentKind::Codex);
        let other = launch("w2", AgentKind::Claude);

        assert_eq!(
            claude,
            Launch {
                agent: AgentKind::Claude,
                program: program(&dir, "claude"),
                config_dir: dir.join("workspaces/w-w1/claude"),
                cwd: PathBuf::from("/work"),
                access: AccessLevel::AskFirst,
            }
        );
        assert_eq!(codex.program, program(&dir, "codex"));
        assert_eq!(codex.config_dir, dir.join("workspaces/w-w1/codex"));
        assert_eq!(other.config_dir, dir.join("workspaces/w-w2/claude"));
        assert_eq!(workspaces.root(&id("w1")), dir.join("workspaces/w-w1"));
        for launch in [claude, codex, other] {
            assert!(launch.config_dir.is_dir());
        }
    }

    #[test]
    fn ids_that_differ_only_in_case_or_name_a_device_get_folders_of_their_own() {
        let names =
            ["ab", "Ab", "aB", "AB", "a-b", "CON", "con"].map(|name| folder_name(&id(name)));

        assert_eq!(
            names,
            [
                "w-ab", "w-.ab", "w-a.b", "w-.a.b", "w-a-b", "w-.c.o.n", "w-con"
            ]
        );
    }

    #[test]
    fn folder_names_turn_back_into_their_ids_and_other_names_into_none() {
        for back in ["ab", "Ab", "aB", "AB", "a-b", "CON", "con"] {
            assert_eq!(workspace_id(&folder_name(&id(back))), Some(id(back)));
        }
        for other in ["ab", "w-", "w-a b", "w-a.", "w-a..b", "w-.1", "w-A", "x-ab"] {
            assert_eq!(workspace_id(other), None, "{other}");
        }
    }

    #[test]
    fn each_known_workspace_says_whether_its_agents_are_signed_in() {
        let dir = TempDir::new().unwrap();
        let workspaces = workspaces(&dir);
        assert_eq!(workspaces.describe(|_| Vec::new()).unwrap(), "");
        for workspace in ["w2", "W1", "a"] {
            workspaces
                .launch(&id(workspace), AgentKind::Claude, PathBuf::from("/work"))
                .unwrap();
        }
        for file in ["notes.txt", "w-file"] {
            fs::write(dir.join("workspaces").join(file), "").unwrap();
        }
        let w2 = dir.join("workspaces/w-w2");

        let sign_in = |launch: &Launch| {
            let in_w2 = launch.cwd == w2 && launch.config_dir.starts_with(&w2);
            match launch.agent {
                AgentKind::Claude if in_w2 => Some(SignIn::SignedIn),
                AgentKind::Codex if in_w2 => Some(SignIn::SignedOut),
                AgentKind::Claude => Some(SignIn::SignedOut),
                AgentKind::Codex | AgentKind::Newer => None,
            }
        };
        let text = workspaces
            .describe(|launches| launches.iter().map(sign_in).collect())
            .unwrap();
        // Answers that run out show as unknown.
        let cut_short = workspaces.describe(|_| vec![Some(SignIn::SignedIn)]);

        assert_eq!(
            text,
            "Workspace W1: access ask-first; Claude Code signed out, Codex unknown\n\
             Workspace a: access ask-first; Claude Code signed out, Codex unknown\n\
             Workspace w2: access ask-first; Claude Code signed in, Codex signed out\n"
        );
        assert!(cut_short.unwrap().starts_with(
            "Workspace W1: access ask-first; Claude Code signed in, Codex unknown\n\
             Workspace a: access ask-first; Claude Code unknown"
        ));
    }

    #[test]
    fn workspaces_that_cant_be_listed_are_an_error_not_an_empty_list() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("workspaces"), "").unwrap();

        let unlisted = Workspaces::new(&dir, None).describe(|_| Vec::new());

        unlisted.unwrap_err();
    }

    #[test]
    fn a_program_that_isnt_on_the_path_is_not_installed_and_never_asked() {
        let dir = TempDir::new().unwrap();
        workspaces(&dir)
            .launch(&id("w1"), AgentKind::Claude, PathBuf::from("/work"))
            .unwrap();
        let none_asked = |asked: &[Launch]| {
            assert_eq!(asked, []);
            Vec::new()
        };
        let not_installed =
            "Workspace w1: access ask-first; Claude Code not installed, Codex not installed\n";

        for path in [None, Some(dir.join("empty").into_os_string())] {
            let workspaces = Workspaces::new(&dir, path);
            let error = workspaces
                .launch(&id("w2"), AgentKind::Codex, PathBuf::from("/work"))
                .unwrap_err();

            assert_eq!(error.code().as_str(), "agent.not_installed");
            assert!(!workspaces.root(&id("w2")).exists());
            assert_eq!(workspaces.describe(none_asked).unwrap(), not_installed);
        }
    }

    fn choose(dir: &Path, workspace: &str, json: &str) {
        fs::create_dir_all(dir.join("workspaces").join(format!("w-{workspace}"))).unwrap();
        fs::create_dir_all(dir.join("settings")).unwrap();
        fs::write(dir.join(format!("settings/w-{workspace}.json")), json).unwrap();
    }

    #[test]
    fn agents_start_only_where_the_access_level_lets_them_and_carry_it() {
        let dir = TempDir::new().unwrap();
        let workspaces = workspaces(&dir);
        choose(&dir, "off", r#"{"access":"off"}"#);
        choose(&dir, "newer", r#"{"access":"sandboxed"}"#);
        choose(&dir, "reading", r#"{"access":"read_only"}"#);
        choose(&dir, "asking", r#"{"access":"ask_first"}"#);
        choose(&dir, "full", r#"{"access":"full","team":true}"#);
        choose(&dir, "broken", "{");
        let launch = |workspace: &str| {
            let launch =
                workspaces.launch(&id(workspace), AgentKind::Claude, PathBuf::from("/work"));
            launch
                .map(|launch| launch.access)
                .map_err(|error| error.code().as_str().to_owned())
        };
        let refused = |code: &str| Err(code.to_owned());

        assert_eq!(launch("off"), refused("access.off"));
        assert_eq!(launch("newer"), refused("access.off"));
        assert_eq!(launch("broken"), refused("workspace.settings_unreadable"));
        assert_eq!(launch("reading"), Ok(AccessLevel::ReadOnly));
        assert_eq!(launch("asking"), Ok(AccessLevel::AskFirst));
        assert_eq!(launch("full"), Ok(AccessLevel::Full));
        assert_eq!(launch("unset"), Ok(AccessLevel::AskFirst));
        assert!(!workspaces.root(&id("off")).join("claude").exists());
        // Signing in doesn't depend on the level.
        let prepared = workspaces.prepared(&id("off"), AgentKind::Claude, dir.to_path_buf());
        assert!(prepared.unwrap().config_dir.is_dir());
    }

    #[test]
    fn each_known_workspace_shows_its_access_level_and_warnings() {
        let dir = TempDir::new().unwrap();
        choose(&dir, "a", r#"{"access":"full","team":true}"#);
        choose(&dir, "b", r#"{"access":"off"}"#);
        choose(&dir, "c", "{");

        let text = Workspaces::new(&dir, None)
            .describe(|_| Vec::new())
            .unwrap();

        assert_eq!(
            text,
            "Workspace a: access full; Claude Code not installed, Codex not installed\n\
             Warning: full access is on in a team workspace. Its agents change files on this \
             machine without asking.\n\
             Workspace b: access off; Claude Code not installed, Codex not installed\n\
             Workspace c: access unknown; Claude Code not installed, Codex not installed\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_folders_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let launch = workspaces(&dir)
            .launch(&id("w1"), AgentKind::Claude, PathBuf::from("/work"))
            .unwrap();

        for folder in [launch.config_dir.as_path(), &dir.join("workspaces")] {
            let mode = fs::metadata(folder).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", folder.display());
        }
    }

    #[test]
    fn an_unknown_agent_or_an_unusable_folder_is_refused() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.join("workspaces/w-w1")).unwrap();
        fs::write(dir.join("workspaces/w-w1/claude"), "").unwrap();
        let workspaces = workspaces(&dir);
        let code = |agent| {
            let error = workspaces
                .launch(&id("w1"), agent, PathBuf::from("/work"))
                .unwrap_err();
            error.code().as_str().to_owned()
        };

        assert_eq!(code(AgentKind::Newer), "agent.unknown");
        assert_eq!(code(AgentKind::Claude), "workspace.folder_failed");
    }

    #[test]
    fn agents_never_work_in_or_above_the_state_folder() {
        let dir = TempDir::new().unwrap();
        let workspaces = workspaces(&dir);
        let code = |cwd: PathBuf| {
            let launch = workspaces.launch(&id("w1"), AgentKind::Claude, cwd);
            launch.map_or_else(|error| error.code().as_str().to_owned(), |_| String::new())
        };
        let parent = dir.parent().unwrap().to_path_buf();

        for refused in [
            dir.to_path_buf(),
            dir.join("settings"),
            dir.join("workspaces/w-w1"),
            parent,
        ] {
            assert_eq!(
                code(refused.clone()),
                "workspace.cwd_refused",
                "{}",
                refused.display()
            );
        }
        assert_eq!(code(dir.with_extension("work")), "");
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_folder_reached_through_a_symlink_is_still_refused() {
        let dir = TempDir::new().unwrap();
        let state = dir.join("state");
        fs::create_dir(&state).unwrap();
        std::os::unix::fs::symlink(&state, dir.join("link")).unwrap();
        let workspaces = workspaces(&state);

        let error = workspaces
            .launch(&id("w1"), AgentKind::Claude, dir.join("link/settings"))
            .unwrap_err();

        assert_eq!(error.code().as_str(), "workspace.cwd_refused");
        // A symlink to nowhere could later point anywhere, so it's refused too.
        std::os::unix::fs::symlink(dir.join("missing"), dir.join("dangling")).unwrap();
        let dangling = workspaces.launch(&id("w1"), AgentKind::Claude, dir.join("dangling"));
        assert_eq!(
            dangling.unwrap_err().code().as_str(),
            "workspace.cwd_refused"
        );
        // A file in the way can't be resolved, so the folder is refused too.
        fs::write(dir.join("file"), "").unwrap();
        let blocked = workspaces.launch(&id("w1"), AgentKind::Claude, dir.join("file/inside"));
        assert_eq!(
            blocked.unwrap_err().code().as_str(),
            "workspace.cwd_refused"
        );
    }

    // `true` and `false` stand in for a vendor's status command.
    #[cfg(unix)]
    #[tokio::test]
    async fn every_sign_in_is_checked_in_order_and_a_failed_check_is_logged_once_and_unknown() {
        let dir = TempDir::new().unwrap();
        let (log, records) = test_support::recording_logger();
        let launch = |program: &str| Launch {
            agent: AgentKind::Codex,
            program: program.into(),
            config_dir: dir.to_path_buf(),
            cwd: dir.to_path_buf(),
            access: AccessLevel::AskFirst,
        };
        let launches = ["true", "/missing/codex", "false"].map(launch);

        let answers = sign_ins(&log, &launches).await;

        assert_eq!(
            answers,
            [Some(SignIn::SignedIn), None, Some(SignIn::SignedOut)]
        );
        assert_eq!(test_support::messages(&records), ["sign-in check failed"]);
    }

    // The script never exits in time. With time paused, the clock moves only when every check
    // is waiting, so it shows how many waits there were, one after another.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn checks_run_a_few_at_once_so_slow_ones_dont_add_up() {
        use std::time::Duration;

        let dir = TempDir::new().unwrap();
        let (log, records) = test_support::recording_logger();
        let program = dir.join("hang");
        // Written from a child shell, so no fork of this process inherits a write handle.
        let script = "printf '#!/bin/sh\\nexec sleep 1000\\n' > \"$1\" && chmod 755 \"$1\"";
        let written = std::process::Command::new("sh")
            .args(["-c", script, "sh"])
            .arg(&program)
            .status()
            .unwrap();
        assert!(written.success());
        let hanging = Launch {
            agent: AgentKind::Claude,
            program,
            config_dir: dir.to_path_buf(),
            cwd: dir.to_path_buf(),
            access: AccessLevel::AskFirst,
        };
        let launches = vec![hanging; AT_ONCE + 1];
        let started = tokio::time::Instant::now();

        let answers = sign_ins(&log, &launches).await;

        let waits = started.elapsed().as_secs() / 30;
        assert_eq!(waits, 2);
        assert!(started.elapsed() < Duration::from_secs(61));
        assert_eq!(answers, vec![None; AT_ONCE + 1]);
        assert_eq!(test_support::messages(&records).len(), AT_ONCE + 1);
    }

    #[test]
    fn each_installed_agent_takes_the_next_answer_in_order() {
        let dir = TempDir::new().unwrap();
        let installed = workspaces(&dir)
            .located(&id("w1"), AgentKind::Claude, dir.to_path_buf())
            .ok();
        let places =
            [&installed, &None, &installed, &installed, &None, &installed].map(Clone::clone);
        let answers = vec![
            Some(SignIn::SignedIn),
            None,
            Some(SignIn::SignedOut),
            Some(SignIn::SignedIn),
        ];

        let shown = states(&places, answers).into_iter().map(state_name);

        assert_eq!(
            shown.collect::<Vec<_>>(),
            [
                "signed in",
                "not installed",
                "unknown",
                "signed out",
                "not installed",
                "signed in"
            ]
        );
        assert_eq!(states(&[installed], Vec::new()), [SignInState::Unknown]);
        assert_eq!(state_name(SignInState::Newer), "unknown");
    }
}
