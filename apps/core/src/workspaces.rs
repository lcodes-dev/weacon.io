//! What this machine keeps for each workspace: a folder of its own, holding each agent's
//! config folder and so its sign-in.

use std::fs;
use std::path::{Path, PathBuf};

use weacon_agents::{Launch, SignIn};
use weacon_observability::Fault;
use weacon_protocol::{AgentKind, WorkspaceId, code};

use crate::platform;

#[derive(Debug)]
pub(crate) struct Workspaces {
    dir: PathBuf,
}

impl Workspaces {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join("workspaces"),
        }
    }

    /// The workspaces this machine has a folder for, sorted by id.
    fn known(&self) -> Vec<WorkspaceId> {
        let mut known: Vec<_> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| workspace_id(entry.file_name().to_str()?))
            .collect();
        known.sort_by(|one, other| one.as_str().cmp(other.as_str()));
        known
    }

    /// A line for each known workspace saying whether each agent is signed in, as `sign_in`
    /// reads it; `None` shows as unknown.
    pub(crate) fn describe(&self, sign_in: impl Fn(&Launch) -> Option<SignIn>) -> String {
        let line = |workspace: WorkspaceId| {
            let root = self.root(&workspace);
            let agents = AGENTS.map(|(agent, name)| {
                let launch = Launch::new(agent, &root, root.clone()).ok();
                format!("{name} {}", state(launch.as_ref().and_then(&sign_in)))
            });
            format!("Workspace {}: {}\n", workspace.as_str(), agents.join(", "))
        };
        self.known().into_iter().map(line).collect()
    }

    /// The folder for everything this machine keeps for `workspace`.
    pub(crate) fn root(&self, workspace: &WorkspaceId) -> PathBuf {
        self.dir.join(folder_name(workspace))
    }

    /// How to start `agent` in `cwd` with `workspace`'s own config folder, created private when
    /// missing; the only place runs get an agent.
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
        let launch = Launch::new(agent, &self.root(workspace), cwd)?;
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

// A capital becomes `.` and its small letter, so ids differing only in case don't share a
// folder; the prefix keeps Windows' device names, such as `CON`, out.
fn folder_name(workspace: &WorkspaceId) -> String {
    let mut name = String::from("w-");
    for letter in workspace.as_str().chars() {
        if letter.is_ascii_uppercase() {
            name.push('.');
        }
        name.push(letter.to_ascii_lowercase());
    }
    name
}

const fn state(sign_in: Option<SignIn>) -> &'static str {
    match sign_in {
        Some(SignIn::SignedIn) => "signed in",
        Some(SignIn::SignedOut) => "signed out",
        Some(SignIn::NotInstalled) => "not installed",
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

    #[test]
    fn each_workspace_and_agent_gets_its_own_config_folder() {
        let dir = TempDir::new().unwrap();
        let workspaces = Workspaces::new(&dir);
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
                program: PathBuf::from("claude"),
                config_dir: dir.join("workspaces/w-w1/claude"),
                cwd: PathBuf::from("/work"),
            }
        );
        assert_eq!(codex.program, PathBuf::from("codex"));
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
        let workspaces = Workspaces::new(&dir);
        assert_eq!(workspaces.describe(|_| Some(SignIn::SignedIn)), "");
        for workspace in ["w2", "W1", "a"] {
            workspaces
                .launch(&id(workspace), AgentKind::Claude, dir.to_path_buf())
                .unwrap();
        }
        for file in ["notes.txt", "w-file"] {
            fs::write(dir.join("workspaces").join(file), "").unwrap();
        }
        let w2 = dir.join("workspaces/w-w2");

        let text = workspaces.describe(|launch| {
            let in_w2 = launch.cwd == w2 && launch.config_dir.starts_with(&w2);
            match launch.agent {
                AgentKind::Claude if in_w2 => Some(SignIn::SignedIn),
                AgentKind::Codex if in_w2 => Some(SignIn::SignedOut),
                AgentKind::Claude => Some(SignIn::NotInstalled),
                AgentKind::Codex | AgentKind::Newer => None,
            }
        });

        assert_eq!(
            text,
            "Workspace W1: Claude Code not installed, Codex unknown\n\
             Workspace a: Claude Code not installed, Codex unknown\n\
             Workspace w2: Claude Code signed in, Codex signed out\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn config_folders_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let launch = Workspaces::new(&dir)
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
        fs::write(dir.join("workspaces"), "").unwrap();
        let workspaces = Workspaces::new(&dir);
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
