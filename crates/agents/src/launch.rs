use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::time::timeout;
use weacon_observability::Fault;
use weacon_protocol::{AccessLevel, AgentKind, AgentSession, App, RunId, code};

use crate::access::Rules;
use crate::platform;
use crate::run::{self, AgentRun};
use crate::spawn::SpawnPolicy;
use crate::terminal::{Events, Size, Terminal};
use crate::vendor::{Vendor, unknown, vendor};

/// How to start one of the person's own agents for one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Launch {
    pub agent: AgentKind,
    /// The vendor's installed program.
    pub program: PathBuf,
    /// The workspace's config folder for this agent, which holds its sign-in.
    pub config_dir: PathBuf,
    /// The folder the agent works in.
    pub cwd: PathBuf,
    /// What the agent may do on this machine, which decides the prompts no person is asked.
    pub access: AccessLevel,
    /// The core's MCP for this run; only Claude Code is pointed at it.
    pub mcp: Option<McpServer>,
    /// A Claude Code plugin folder for this run, given only when absolute.
    pub plugin: Option<PathBuf>,
}

/// Where the core's MCP listens, and the token of the one run it serves.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServer {
    pub url: String,
    pub token: String,
    /// The names of its tools, which all only read.
    pub tools: Vec<String>,
}

impl fmt::Debug for McpServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("McpServer")
            .field("url", &self.url)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

// A status command that hangs is given up on, so a report still prints.
const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
// So a status command that prints without end can't fill memory.
const STATUS_LIMIT: u64 = 64 * 1024;

/// Whether an agent is signed in for a workspace, as the vendor's own status command says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignIn {
    SignedIn,
    SignedOut,
}

impl Launch {
    /// `agent` in `cwd`, with the vendor's program where `find` says it is installed, and a
    /// config folder named after it in `workspace_dir`.
    ///
    /// # Errors
    /// When this build doesn't know the agent, or its program isn't installed.
    pub fn new(
        agent: AgentKind,
        find: impl FnOnce(&str) -> Option<PathBuf>,
        workspace_dir: &Path,
        cwd: PathBuf,
    ) -> Result<Self, Fault> {
        let name = agent.name().ok_or_else(unknown)?;
        Ok(Self {
            agent,
            program: find(name).ok_or_else(not_installed)?,
            config_dir: workspace_dir.join(name),
            cwd,
            access: AccessLevel::AskFirst,
            mcp: None,
            plugin: None,
        })
    }

    /// Starts the agent under `policy`, resuming `session` when given. `app` is Weacon's own
    /// build, for agents that record their client.
    ///
    /// # Errors
    /// When this build doesn't know the agent, or its program can't be started.
    pub fn start(
        &self,
        policy: &SpawnPolicy,
        app: &App,
        run: RunId,
        session: Option<&AgentSession>,
    ) -> Result<AgentRun, Fault> {
        let (command, wire) = (self.vendor()?.drive)(self, app, session);
        let rules = Rules {
            level: self.access,
            cwd: self.cwd.clone(),
            workspace: self
                .config_dir
                .parent()
                .unwrap_or(&self.config_dir)
                .to_path_buf(),
        };
        run::spawn(command, policy, run, wire, rules)
    }

    /// Runs the vendor's own status command and reads how it exits and, for Claude, only where
    /// the sign-in comes from: signed in only from the workspace's own config folder.
    ///
    /// # Errors
    /// When this build doesn't know the agent, or the command can't be run or doesn't end.
    pub async fn sign_in(&self) -> Result<SignIn, Fault> {
        let vendor = self.vendor()?;
        let mut command = self.status_command(vendor);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let failed = || {
            Fault::new(
                code!("agent.status_failed"),
                "The agent's sign-in couldn't be checked",
            )
        };
        match timeout(STATUS_TIMEOUT, status_of(command)).await {
            Ok(Ok((exit, status))) if exit.success() && (vendor.own_sign_in)(&status) => {
                Ok(SignIn::SignedIn)
            }
            Ok(Ok(_)) => Ok(SignIn::SignedOut),
            Ok(Err(error)) => Err(failed().because(error)),
            Err(elapsed) => Err(failed().because(elapsed)),
        }
    }

    /// Refuses unless the agent is signed in only from the workspace's own config folder,
    /// checked afresh on every call.
    ///
    /// # Errors
    /// `agent.signed_out` when it isn't; the check's own fault when it can't run.
    pub async fn require_sign_in(&self) -> Result<(), Fault> {
        match self.sign_in().await? {
            SignIn::SignedIn => Ok(()),
            SignIn::SignedOut => Err(Fault::new(
                code!("agent.signed_out"),
                "The agent isn't signed in for this workspace on this machine",
            )),
        }
    }

    /// Runs the vendor's own sign-in in the terminal this process has, for the person to
    /// finish there. Nothing that passes between them is read.
    ///
    /// # Errors
    /// When this build doesn't know the agent, its program can't be run, or the sign-in ends
    /// without success.
    pub async fn log_in(&self) -> Result<(), Fault> {
        let mut command = self.login_command(self.vendor()?);
        let failed = || Fault::new(code!("agent.login_failed"), "The sign-in didn't finish");
        match command.status().await {
            Ok(exit) if exit.success() => Ok(()),
            Ok(_) => Err(failed()),
            Err(error) => Err(failed().because(error)),
        }
    }

    /// Runs the agent on `session` in the terminal this process has, for the person to carry on
    /// by hand: the vendor's program unmodified, with the workspace's config folder and no
    /// shell. Only Claude Code, whose `--resume` takes the session a chat holds.
    ///
    /// # Errors
    /// When this agent can't be resumed by hand, or its program can't be run.
    pub async fn resume_by_hand(&self, session: &AgentSession) -> Result<(), Fault> {
        let resume = self.vendor()?.resume.ok_or_else(|| {
            Fault::new(
                code!("agent.no_resume_by_hand"),
                "This agent can't be carried on by hand yet",
            )
        })?;
        let mut command = resume(self, session);
        let failed = |error| {
            Fault::new(code!("agent.resume_failed"), "The agent couldn't start").because(error)
        };
        // The terminal's Ctrl-C reaches this process too; caught, it leaves the agent the terminal.
        let mut interrupts = platform::interrupts().map_err(failed)?;
        let mut child = command.spawn().map_err(failed)?;
        loop {
            tokio::select! {
                status = child.wait() => return status.map(drop).map_err(failed),
                Some(()) = interrupts.recv() => {}
            }
        }
    }

    /// Runs the vendor's own sign-in in a new terminal of `size`: the same program, fixed
    /// arguments and config folder as [`Launch::log_in`], and no shell.
    ///
    /// # Errors
    /// When this build doesn't know the agent, or its program can't be started.
    pub fn log_in_terminal(&self, size: Size) -> Result<(Terminal, Events), Fault> {
        let mut command = self.login_command(self.vendor()?);
        // What the browser's terminal emulates, whatever this process runs under.
        command.env("TERM", "xterm-256color");
        Terminal::open(command, size).map_err(|error| {
            Fault::new(code!("agent.login_failed"), "The sign-in couldn't start").because(error)
        })
    }

    /// Whether a run of this agent is pointed at the core's MCP.
    #[must_use]
    pub fn takes_mcp(&self) -> bool {
        self.vendor().is_ok_and(|vendor| vendor.takes_mcp)
    }

    pub(crate) fn with_workspace_sign_in(&self, vendor: &Vendor) -> Command {
        let mut command = Command::new(&self.program);
        let inherited = std::env::vars_os().map(|(name, _)| name);
        vendor.other_sign_ins.remove(&mut command, inherited);
        command.env(vendor.config_variable, &self.config_dir);
        command
    }

    pub(crate) fn status_command(&self, vendor: &Vendor) -> Command {
        let mut command = self.with_workspace_sign_in(vendor);
        command.args(vendor.status);
        command
    }

    pub(crate) fn login_command(&self, vendor: &Vendor) -> Command {
        let mut command = self.with_workspace_sign_in(vendor);
        command.args(vendor.login);
        command
    }

    fn vendor(&self) -> Result<&'static Vendor, Fault> {
        vendor(self.agent)
    }
}

/// Variables that would sign a vendor's program in other than through the workspace's own
/// config folder: any whose name starts with one of `prefixes` or is one of `names`.
pub(crate) struct OtherSignIns {
    pub(crate) prefixes: &'static [&'static str],
    pub(crate) names: &'static [&'static str],
}

impl OtherSignIns {
    /// Removes from `command` those of the `inherited` variables' names that match.
    pub(crate) fn remove(&self, command: &mut Command, inherited: impl Iterator<Item = OsString>) {
        for name in inherited.filter(|name| self.matches(name)) {
            command.env_remove(name);
        }
    }

    // Windows reads variable names in any case.
    pub(crate) fn matches(&self, name: &OsStr) -> bool {
        let Some(name) = name.to_str().map(str::to_ascii_uppercase) else {
            return false;
        };
        self.prefixes.iter().any(|prefix| name.starts_with(prefix))
            || self.names.contains(&name.as_str())
    }
}

// Runs a status command and keeps the start of what it prints; closing the pipe stops the
// rest.
async fn status_of(mut command: Command) -> io::Result<(ExitStatus, Vec<u8>)> {
    let mut child = command.spawn()?;
    let mut status = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        stdout.take(STATUS_LIMIT).read_to_end(&mut status).await?;
    }
    Ok((child.wait().await?, status))
}

fn not_installed() -> Fault {
    Fault::new(
        code!("agent.not_installed"),
        "The agent's program isn't installed",
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, app, run_id};
    use crate::wire::Wire;

    fn launch(agent: AgentKind, dir: &TempDir) -> Launch {
        test_support::launch(agent, dir.join("agent"), dir)
    }

    fn start(launch: &Launch) -> Result<AgentRun, Fault> {
        launch.start(&SpawnPolicy::default(), &app(), run_id("run-1"), None)
    }

    const SESSION: &str = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

    #[cfg(unix)]
    #[tokio::test]
    async fn a_resumed_agent_keeps_the_terminal_through_ctrl_c_until_the_person_ends_it() {
        let dir = TempDir::new().unwrap();
        let claude = launch(AgentKind::Claude, &dir);
        // Ctrl-C reaches the whole foreground group, this process too.
        let script = "#!/bin/sh\nkill -INT $PPID\nprintf ended > ended\nexit 3\n";
        weacon_testing::platform::write_program(&claude.program, script).unwrap();
        let session = AgentSession::try_from(SESSION.to_owned()).unwrap();
        // Whatever the agent exits with, the person ended it.
        claude.resume_by_hand(&session).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("ended")).unwrap(), "ended");
        let missing = Launch {
            program: dir.join("missing"),
            ..claude
        };
        let fault = missing.resume_by_hand(&session).await.unwrap_err();
        assert_eq!(fault.code().as_str(), "agent.resume_failed");
    }

    #[tokio::test]
    async fn only_claude_is_resumed_by_hand_for_now() {
        let dir = TempDir::new().unwrap();
        let session = AgentSession::try_from(SESSION.to_owned()).unwrap();
        let codex = launch(AgentKind::Codex, &dir)
            .resume_by_hand(&session)
            .await;
        assert_eq!(
            codex.unwrap_err().code().as_str(),
            "agent.no_resume_by_hand"
        );
        let newer = launch(AgentKind::Newer, &dir)
            .resume_by_hand(&session)
            .await;
        assert_eq!(newer.unwrap_err().code().as_str(), "agent.unknown");
    }

    #[test]
    fn inherited_variables_with_a_sign_in_prefix_or_name_are_removed_in_any_case() {
        let other = OtherSignIns {
            prefixes: &["VENDOR_"],
            names: &["TOKEN"],
        };
        // No two differ only in case, as Windows keeps one of each.
        let inherited = ["VENDOR_KEY", "vendor_id", "token", "VENDOR", "PATH"];
        let mut command = Command::new("agent");
        command.env("VENDOR_HOME", "/config");

        other.remove(&mut command, inherited.into_iter().map(OsString::from));

        let envs: BTreeMap<_, _> = command
            .as_std()
            .get_envs()
            .map(|(name, value)| (name.to_ascii_uppercase(), value))
            .collect();
        let removed = |name: &str| (OsString::from(name), None);
        assert_eq!(
            envs,
            BTreeMap::from([
                removed("TOKEN"),
                removed("VENDOR_ID"),
                removed("VENDOR_KEY"),
                (OsString::from("VENDOR_HOME"), Some(OsStr::new("/config"))),
            ])
        );
    }

    #[test]
    fn only_claude_code_is_pointed_at_the_mcp() {
        let dir = TempDir::new().unwrap();
        let takes = |agent| launch(agent, &dir).takes_mcp();

        assert!(takes(AgentKind::Claude));
        assert!(!takes(AgentKind::Codex));
        assert!(!takes(AgentKind::Newer));
    }

    #[test]
    fn each_agent_starts_through_its_own_driver() {
        let dir = TempDir::new().unwrap();
        let first_arg = |agent| {
            let launch = launch(agent, &dir);
            let (command, wire) = (launch.vendor().unwrap().drive)(&launch, &app(), None);
            let command = command.as_std();
            assert_eq!(command.get_program(), dir.join("agent"));
            (command.get_args().next().map(ToOwned::to_owned), wire)
        };

        let (claude, claude_wire) = first_arg(AgentKind::Claude);
        let (codex, codex_wire) = first_arg(AgentKind::Codex);

        assert_eq!(claude.as_deref(), Some("--print".as_ref()));
        assert_eq!(codex.as_deref(), Some("app-server".as_ref()));
        assert!(matches!(claude_wire, Wire::Claude(_)));
        assert!(matches!(codex_wire, Wire::Codex(_)));
    }

    #[test]
    fn a_launch_has_the_vendors_installed_program_and_its_folder_in_the_workspace() {
        let installed = |name: &str| Some(Path::new("/opt/bin").join(name));
        let new = |agent, find: fn(&str) -> Option<PathBuf>| {
            Launch::new(agent, find, Path::new("/state/w"), PathBuf::from("/work"))
        };
        let claude = new(AgentKind::Claude, installed).unwrap();
        let codex = new(AgentKind::Codex, installed).unwrap();
        let code = |launch: Result<Launch, Fault>| launch.unwrap_err().code().as_str().to_owned();

        assert_eq!(
            claude,
            Launch {
                agent: AgentKind::Claude,
                program: PathBuf::from("/opt/bin/claude"),
                config_dir: PathBuf::from("/state/w/claude"),
                cwd: PathBuf::from("/work"),
                access: AccessLevel::AskFirst,
                mcp: None,
                plugin: None,
            }
        );
        assert_eq!(
            (codex.agent, codex.program, codex.config_dir),
            (
                AgentKind::Codex,
                "/opt/bin/codex".into(),
                "/state/w/codex".into()
            )
        );
        assert_eq!(code(new(AgentKind::Newer, installed)), "agent.unknown");
        assert_eq!(
            code(new(AgentKind::Claude, |_| None)),
            "agent.not_installed"
        );
    }

    #[tokio::test]
    async fn an_agent_that_cant_start_is_reported() {
        let dir = TempDir::new().unwrap();
        let code = |agent| {
            start(&launch(agent, &dir))
                .err()
                .unwrap()
                .code()
                .as_str()
                .to_owned()
        };

        assert_eq!(code(AgentKind::Claude), "agent.not_started");
        assert_eq!(code(AgentKind::Codex), "agent.not_started");
        assert_eq!(code(AgentKind::Newer), "agent.unknown");
    }

    async fn sign_in_code(launch: Launch) -> String {
        let error = launch.sign_in().await.unwrap_err();
        error.code().as_str().to_owned()
    }

    #[tokio::test]
    async fn a_program_that_is_gone_or_an_unknown_agent_is_an_error_not_a_sign_in() {
        let dir = TempDir::new().unwrap();
        let code = |agent| sign_in_code(launch(agent, &dir));

        assert_eq!(code(AgentKind::Claude).await, "agent.status_failed");
        assert_eq!(code(AgentKind::Codex).await, "agent.status_failed");
        assert_eq!(code(AgentKind::Newer).await, "agent.unknown");
    }

    // `true` and `false` stand in for a vendor's status command: they exit the same whatever
    // their arguments, and print nothing, which never names Claude's sign-in.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_status_commands_exit_says_whether_the_agent_is_signed_in() {
        let dir = TempDir::new().unwrap();
        let with = |agent, program: &str| test_support::launch(agent, program, &dir);
        let cases = [
            (AgentKind::Codex, "true", SignIn::SignedIn),
            (AgentKind::Codex, "false", SignIn::SignedOut),
            (AgentKind::Claude, "true", SignIn::SignedOut),
            (AgentKind::Claude, "false", SignIn::SignedOut),
        ];

        for (agent, program, expected) in cases {
            let answer = with(agent, program).sign_in().await.unwrap();
            assert_eq!(answer, expected, "{agent:?} {program}");
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_program_that_cant_be_run_is_an_error_not_a_sign_in() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.join("agent"), "").unwrap();

        let error = launch(AgentKind::Claude, &dir).sign_in().await.unwrap_err();

        assert_eq!(error.code().as_str(), "agent.status_failed");
    }

    // `yes` never exits; with time paused, the wait for it ends as soon as nothing else can run.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn a_status_command_that_never_ends_is_given_up_on() {
        let dir = TempDir::new().unwrap();
        let hanging = test_support::launch(AgentKind::Codex, "yes", &dir);

        let error = hanging.sign_in().await.unwrap_err();

        assert_eq!(error.code().as_str(), "agent.status_failed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_run_is_refused_unless_the_workspace_is_signed_in_on_its_own() {
        let dir = TempDir::new().unwrap();
        let with = |program: &str| test_support::launch(AgentKind::Codex, program, &dir);

        let refused = with("false").require_sign_in().await.unwrap_err();

        with("true").require_sign_in().await.unwrap();
        assert_eq!(refused.code().as_str(), "agent.signed_out");
    }

    async fn log_in_code(launch: Launch) -> Option<String> {
        Some(launch.log_in().await.err()?.code().as_str().to_owned())
    }

    #[tokio::test]
    async fn a_sign_in_needs_a_known_agent_and_a_program_that_is_still_there() {
        let dir = TempDir::new().unwrap();
        let code = |agent| log_in_code(launch(agent, &dir));

        assert_eq!(
            code(AgentKind::Claude).await.as_deref(),
            Some("agent.login_failed")
        );
        assert_eq!(
            code(AgentKind::Codex).await.as_deref(),
            Some("agent.login_failed")
        );
        assert_eq!(
            code(AgentKind::Newer).await.as_deref(),
            Some("agent.unknown")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_sign_in_ends_as_the_vendors_program_does() {
        let dir = TempDir::new().unwrap();
        let with = |program: &str| test_support::launch(AgentKind::Claude, program, &dir);
        // A file that can't be run.
        std::fs::write(dir.join("agent"), "").unwrap();

        assert_eq!(log_in_code(with("true")).await, None);
        assert_eq!(
            log_in_code(with("false")).await.as_deref(),
            Some("agent.login_failed")
        );
        assert_eq!(
            log_in_code(launch(AgentKind::Codex, &dir)).await.as_deref(),
            Some("agent.login_failed")
        );
    }
}
