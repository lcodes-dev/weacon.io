//! What this machine keeps for each workspace: a folder of its own, holding each agent's
//! config folder and so its sign-in.

mod access;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use weacon_agents::{Launch, SignIn};
use weacon_observability::Fault;
use weacon_protocol::{AccessLevel, AgentKind, WorkspaceId, code};

use access::Settings;
pub(crate) use access::run as access;

use crate::app::{App, state_unavailable};
use crate::daemon::runtime;
use crate::platform;

/// Runs the vendor's own sign-in for `agent` in this terminal, against `workspace`'s config
/// folder.
pub(crate) fn login(app: &App, agent: AgentKind, workspace: &WorkspaceId) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let workspaces = Workspaces::new(state_dir, app.path.clone());
    let launch = workspaces.prepared(workspace, agent, workspaces.root(workspace))?;
    runtime()?.block_on(launch.log_in())?;
    let ids = [("workspace_id", workspace.as_str().into())];
    app.log.info("login finished", &ids);
    Ok(())
}

#[derive(Debug)]
pub(crate) struct Workspaces {
    dir: PathBuf,
    path: Option<OsString>,
}

impl Workspaces {
    pub(crate) fn new(state_dir: &Path, path: Option<OsString>) -> Self {
        Self {
            dir: state_dir.join("workspaces"),
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
        // One place for each workspace and agent; a program that isn't installed has no launch
        // to ask, which is the only way the lookup fails for a known agent.
        let places: Vec<Option<Launch>> = known
            .iter()
            .flat_map(|workspace| {
                let root = self.root(workspace);
                AGENTS.map(|(agent, _)| self.located(workspace, agent, root.clone()).ok())
            })
            .collect();
        let asked: Vec<Launch> = places.iter().flatten().cloned().collect();
        let mut answers = sign_ins(&asked).into_iter();
        let mut states = places.iter().map(|place| match place {
            Some(_) => state(answers.next().flatten()),
            None => "not installed",
        });
        let lines = known.iter().map(|workspace| {
            let agents =
                AGENTS.map(|(_, name)| format!("{name} {}", states.next().unwrap_or(state(None))));
            let (id, agents) = (workspace.as_str(), agents.join(", "));
            match Settings::read(&self.root(workspace)) {
                Ok(settings) => {
                    let (level, warning) = (settings.level(), settings.warning());
                    format!("Workspace {id}: access {level}; {agents}\n{warning}")
                }
                Err(_) => format!("Workspace {id}: access unknown; {agents}\n"),
            }
        });
        Ok(lines.collect())
    }

    /// The folder for everything this machine keeps for `workspace`.
    pub(crate) fn root(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir.join(folder_name(workspace))
    }

    /// How to start `agent` in `cwd` with `workspace`'s own config folder; the only place runs
    /// get an agent, and none when the workspace's access level doesn't let agents run.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "runs start their agents through this")
    )]
    pub(crate) fn launch(
        &self,
        workspace: &WorkspaceId,
        agent: AgentKind,
        cwd: PathBuf,
    ) -> Result<Launch, Fault> {
        let access = Settings::read(&self.root(workspace))?.access;
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

const AGENTS: [(AgentKind, &str); 2] = [
    (AgentKind::Claude, "Claude Code"),
    (AgentKind::Codex, "Codex"),
];

fn folder_name(workspace: &WorkspaceId) -> String {
    platform::case_safe_name("w-", workspace.as_str())
}

const fn state(sign_in: Option<SignIn>) -> &'static str {
    match sign_in {
        Some(SignIn::SignedIn) => "signed in",
        Some(SignIn::SignedOut) => "signed out",
        None => "unknown",
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
                .launch(&id(workspace), AgentKind::Claude, dir.to_path_buf())
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
            .launch(&id("w1"), AgentKind::Claude, dir.to_path_buf())
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
                .launch(&id("w2"), AgentKind::Codex, dir.to_path_buf())
                .unwrap_err();

            assert_eq!(error.code().as_str(), "agent.not_installed");
            assert!(!workspaces.root(&id("w2")).exists());
            assert_eq!(workspaces.describe(none_asked).unwrap(), not_installed);
        }
    }

    fn choose(dir: &Path, workspace: &str, json: &str) {
        let root = dir.join("workspaces").join(format!("w-{workspace}"));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("workspace.json"), json).unwrap();
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
            let launch = workspaces.launch(&id(workspace), AgentKind::Claude, dir.to_path_buf());
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
            .launch(&id("w1"), AgentKind::Claude, dir.to_path_buf())
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
                .launch(&id("w1"), agent, dir.to_path_buf())
                .unwrap_err();
            error.code().as_str().to_owned()
        };

        assert_eq!(code(AgentKind::Newer), "agent.unknown");
        assert_eq!(code(AgentKind::Claude), "workspace.folder_failed");
    }
}
