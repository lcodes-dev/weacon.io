//! `weacon update`: the core replaces its own binary with the latest release's build.

mod binary;
mod fetch;
#[cfg(test)]
mod fixtures;
mod install;
mod leave;
mod settings;
mod verify;
mod watch;

pub(crate) use leave::LEAVE;
pub(crate) use watch::schedule;

use std::fs::{File, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use weacon_appkit::Ending;
use weacon_observability::Fault;
use weacon_protocol::{
    BuildTarget, Commit, InstallAction, PROTOCOL, ReleaseManifest, ReleaseVersion, UpdateSchedule,
    code,
};

use crate::app::{App, state_unavailable};
use crate::cli::{UpdateAction, UpdateCommand};
use crate::daemon::{build, socket_path};
use crate::platform::Env;
use crate::service;
use binary::{status_names, version_of};
use install::Paths;
use leave::Report;
use settings::Settings;

/// The target this binary was built for, as release manifests name it.
const TARGET: &str = env!("WEACON_TARGET");

/// How often, and how far apart, a restarted daemon is asked for its build: 30 s in all.
const HEALTH_TRIES: u32 = 60;
const HEALTH_PAUSE: Duration = Duration::from_millis(500);

/// How often, and how far apart, the update lock is tried before it counts as taken: 1 s.
const LOCK_TRIES: u32 = 50;
const LOCK_PAUSE: Duration = Duration::from_millis(20);
/// What an update reaches outside the core; tests pass fakes.
pub(crate) struct World<'a> {
    /// The latest release, once its signature checks out.
    pub(crate) release: &'a dyn Fn() -> Result<ReleaseManifest, Fault>,
    pub(crate) get: fetch::Get<'a>,
    pub(crate) target: &'a str,
    pub(crate) exe: PathBuf,
    /// What a binary prints for `--version`.
    pub(crate) version_of: &'a dyn Fn(&Path) -> io::Result<String>,
    /// Restarts the daemon's service; none when no service runs it.
    pub(crate) restart: Option<&'a dyn Fn() -> Result<(), Fault>>,
    /// Whether the restarted daemon reports `commit` in time.
    pub(crate) healthy: &'a dyn Fn(&Commit) -> bool,
    /// Leave from the running daemon to restart it; none while a chat is busy.
    pub(crate) leave: &'a dyn Fn() -> Result<Option<Report>, Fault>,
}

/// Runs `weacon update` against the real world.
pub(crate) fn run(
    app: &App,
    command: UpdateCommand,
    env: Env<'_>,
    out: &mut impl Write,
) -> Result<Ending, Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let exe = std::env::current_exe().map_err(|error| no_exe().because(error))?;
    let download = fetch::https(fetch::DOWNLOAD_WITHIN);
    let restart = || service::restart(&mut service::system);
    let healthy = |commit: &Commit| status_names(&exe, commit, HEALTH_TRIES, HEALTH_PAUSE);
    let restart: &dyn Fn() -> Result<(), Fault> = &restart;
    let leave = || leave::ask(&socket_path(state_dir), build(&app.version));
    let world = World {
        release: &latest,
        get: &download,
        target: TARGET,
        exe: exe.clone(),
        version_of: &version_of,
        restart: service::installed(&exe, state_dir, env).then_some(restart),
        healthy: &healthy,
        leave: &leave,
    };
    let own = app.version.version.parse().map_err(|_| {
        Fault::new(
            code!("update.no_version"),
            "This build's version can't be read",
        )
    })?;
    update(state_dir, command, own, &world, out)
}

/// Runs `command` for a core at version `own`, keeping its settings in `state_dir`.
pub(crate) fn update(
    state_dir: &Path,
    command: UpdateCommand,
    own: ReleaseVersion,
    world: &World<'_>,
    out: &mut impl Write,
) -> Result<Ending, Fault> {
    let file = settings::file(state_dir);
    let _lock = lock(state_dir)?;
    if let Some(UpdateAction::Schedule { schedule }) = command.action {
        // A broken file mustn't keep its owner from turning updates off.
        let settings = Settings {
            schedule,
            ..Settings::read(&file).unwrap_or_default()
        };
        settings.write(&file)?;
        return say(out, &format!("Updates: {}.", schedule_name(schedule))).map(Ending::from);
    }
    let mut settings = Settings::read(&file)?;
    let release = (world.release)()?;
    settings.accept(&release)?;
    settings.write(&file)?;
    let version = release.version;
    if version <= own {
        return say(out, "weacon is up to date.").map(Ending::from);
    }
    if command.action == Some(UpdateAction::Check) {
        say(
            out,
            &format!("weacon {version} is out. Run `weacon update` to install it."),
        )?;
        return Ok(Ending::Findings);
    }
    if release.protocol != PROTOCOL && !command.yes {
        say(
            out,
            &format!(
                "weacon {version} speaks a new protocol, so it may not reach a server that \
                 hasn't been updated. Run `weacon update --yes` to install it anyway."
            ),
        )?;
        return Ok(Ending::Findings);
    }
    let installed = match install_release(&release, world)? {
        Installed::Running => "The daemon runs it now.",
        Installed::Waiting => "Restart the daemon to run it.",
        Installed::Refused => return refused(&mut settings, &file, version, out),
    };
    say(out, &format!("Installed weacon {version}. {installed}")).map(Ending::from)
}

// Says nothing was installed for a busy chat. Counted as never checked, so the daemon's
// schedule tries again within the hour.
fn refused(
    settings: &mut Settings,
    file: &Path,
    version: ReleaseVersion,
    out: &mut impl Write,
) -> Result<Ending, Fault> {
    settings.last_checked = 0;
    settings.write(file)?;
    say(
        out,
        &format!(
            "A chat is busy, so weacon {version} wasn't installed. Run `weacon update` again \
             once its agent is done."
        ),
    )?;
    Ok(Ending::Findings)
}

/// Where an install left the daemon.
#[derive(Debug, PartialEq, Eq)]
enum Installed {
    /// Restarted on the new build.
    Running,
    /// Still on the old build until it is restarted.
    Waiting,
    /// A chat was busy, so nothing was installed.
    Refused,
}

fn install_release(release: &ReleaseManifest, world: &World<'_>) -> Result<Installed, Fault> {
    let build = BuildTarget::try_from(world.target.to_owned())
        .ok()
        .and_then(|target| release.build_for(&target).cloned())
        .ok_or_else(|| {
            Fault::new(
                code!("update.no_build"),
                "The release has no build for this machine",
            )
        })?;
    let paths = Paths::beside(&world.exe).ok_or_else(no_exe)?;
    // An update of this binary from another state folder fails as busy.
    let _installing = held(&paths.lock, |error| {
        Fault::new(
            code!("update.binary_unwritable"),
            "The update lock couldn't be made beside the binary",
        )
        .because(error)
    })?;
    // Another state folder's update may have put a newer build in place while this waited.
    if installed_version(world, &paths.exe).is_some_and(|installed| release.version <= installed) {
        return Err(Fault::new(
            code!("update.superseded"),
            "A build as new or newer was installed meanwhile",
        ));
    }
    install::stage(&paths, &mut (world.get)(build.url.as_str())?, &build)?;
    install::check_version(&paths, &release.commit, (world.version_of)(&paths.new))?;
    let Some(restart) = world.restart else {
        install::swap(&paths)?;
        return Ok(Installed::Waiting);
    };
    let mut report = match (world.leave)() {
        Ok(Some(report)) => report,
        asked => {
            install::discard(&paths);
            return asked.map(|_| Installed::Refused);
        }
    };
    if let Err(fault) = install::swap(&paths) {
        report(InstallAction::Failed);
        return Err(fault);
    }
    // Leave that ran out may have let a turn start, so the daemon carries on until restarted.
    if !report(InstallAction::Restarting) {
        return Ok(Installed::Waiting);
    }
    restarted(release, &paths, restart, world, report)
}

// Restarts the daemon on the build just swapped in, and puts the old one back when it doesn't
// come up.
fn restarted(
    release: &ReleaseManifest,
    paths: &Paths,
    restart: &dyn Fn() -> Result<(), Fault>,
    world: &World<'_>,
    mut report: Report,
) -> Result<Installed, Fault> {
    let restarted = restart();
    if restarted.is_ok() && (world.healthy)(&release.commit) {
        return Ok(Installed::Running);
    }
    let rolled_back = install::roll_back(paths);
    // Whichever build is in place, starting it again is all that's left to try.
    drop(restart());
    // A daemon the restarts never reached lets its turns go on.
    report(InstallAction::Failed);
    rolled_back?;
    let unhealthy = Fault::new(
        code!("update.unhealthy"),
        "The new build didn't start, so the old one is back",
    );
    Err(match restarted {
        Err(cause) => unhealthy.because(cause),
        Ok(()) => unhealthy,
    })
}

/// Holds the update lock until dropped; whoever comes second fails with `update.busy`.
fn lock(state_dir: &Path) -> Result<File, Fault> {
    held(&state_dir.join("update.lock"), |error| {
        Fault::new(
            code!("update.state_unwritable"),
            "The update lock couldn't be made in the state folder",
        )
        .because(error)
    })
}

/// Holds the lock in the file `path` until dropped, or fails with `unwritable` when it can't
/// make it.
fn held(path: &Path, unwritable: fn(io::Error) -> Fault) -> Result<File, Fault> {
    let file = File::create(path).map_err(unwritable)?;
    // A process forked just then holds the lock until it starts its program, so wait a moment.
    for _ in 0..LOCK_TRIES {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) => std::thread::sleep(LOCK_PAUSE),
            Err(TryLockError::Error(error)) => return Err(unwritable(error)),
        }
    }
    Err(Fault::new(
        code!("update.busy"),
        "Another update is running",
    ))
}

// The version the binary at `exe` says it is, as `weacon 1.2.3 (commit)`; none when it can't say.
fn installed_version(world: &World<'_>, exe: &Path) -> Option<ReleaseVersion> {
    let said = (world.version_of)(exe).ok()?;
    said.split_whitespace().nth(1)?.parse().ok()
}

/// A line saying which newer release the daemon found, if it found one.
pub(crate) fn offer(state_dir: &Path, own: &str) -> Option<String> {
    let own: ReleaseVersion = own.parse().ok()?;
    let offered = Settings::read(&settings::file(state_dir)).ok()?.offered?;
    (offered > own)
        .then(|| format!("Update: weacon {offered} is out. Run `weacon update` to install it."))
}

/// The latest release, fetched and checked against the keys every build trusts.
pub(crate) fn latest() -> Result<ReleaseManifest, Fault> {
    fetch::release(&fetch::https(fetch::MANIFEST_WITHIN), &verify::TRUSTED)
}

const fn schedule_name(schedule: UpdateSchedule) -> &'static str {
    match schedule.effective() {
        UpdateSchedule::Off => "off",
        UpdateSchedule::Notify | UpdateSchedule::Newer => "say when a release is out",
        UpdateSchedule::Daily => "install each day",
        UpdateSchedule::Weekly => "install each week",
    }
}

fn no_exe() -> Fault {
    Fault::new(
        code!("update.no_exe"),
        "The weacon binary couldn't be found",
    )
}

fn say(out: &mut impl Write, line: &str) -> Result<(), Fault> {
    writeln!(out, "{line}").map_err(|error| {
        Fault::new(
            code!("update.output_failed"),
            "The update's result couldn't be printed",
        )
        .because(error)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::io::Cursor;
    use std::sync::{Arc, Mutex};

    use sha2::{Digest, Sha256};
    use weacon_protocol::{DownloadUrl, ReleaseBuild};
    use weacon_testing::TempDir;

    use super::*;

    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";
    const LINUX: &str = "x86_64-unknown-linux-musl";

    fn release(version: &str, published_at: u64, protocol: u32) -> ReleaseManifest {
        let body = b"new";
        ReleaseManifest {
            version: version.parse().unwrap(),
            commit: Commit::try_from(COMMIT.to_owned()).unwrap(),
            published_at,
            protocol,
            builds: vec![ReleaseBuild {
                target: BuildTarget::try_from(LINUX.to_owned()).unwrap(),
                url: DownloadUrl::try_from("https://example.com/weacon".to_owned()).unwrap(),
                size: 3,
                sha256: weacon_protocol::Sha256::from_bytes(Sha256::digest(body).into()),
            }],
        }
    }

    // What `binary --version` prints: the running one says `installed`, a staged one the release.
    fn says(binary: &Path, installed: &str) -> String {
        let version = if binary.ends_with("bin/weacon") {
            installed
        } else {
            "1.2.3"
        };
        format!("weacon {version} ({COMMIT})\n")
    }

    /// A state folder, and a running binary `bin/weacon` that holds `old`.
    struct Machine {
        dir: TempDir,
        restarts: Cell<u32>,
        restart_fails: bool,
        /// The version `bin/weacon` says it is.
        installed: &'static str,
        /// What the daemon says when asked for leave: given, refused or no answer.
        leave: Result<bool, ()>,
        /// Whether the daemon takes a report, as it doesn't once its leave ran out.
        takes: bool,
        asked: Cell<u32>,
        reports: Arc<Mutex<Vec<InstallAction>>>,
    }

    impl Machine {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            fs::create_dir(dir.join("bin")).unwrap();
            fs::write(dir.join("bin/weacon"), "old").unwrap();
            Self {
                dir,
                restarts: Cell::new(0),
                restart_fails: false,
                installed: "1.0.0",
                leave: Ok(true),
                takes: true,
                asked: Cell::new(0),
                reports: Arc::default(),
            }
        }

        fn binary(&self) -> String {
            fs::read_to_string(self.dir.join("bin/weacon")).unwrap()
        }

        fn reports(&self) -> Vec<InstallAction> {
            self.reports.lock().unwrap().clone()
        }

        fn ask(&self) -> Result<Option<Report>, Fault> {
            self.asked.set(self.asked.get() + 1);
            let (reports, takes) = (Arc::clone(&self.reports), self.takes);
            let report: Report = Box::new(move |action| {
                reports.lock().unwrap().push(action);
                takes
            });
            match self.leave {
                Ok(given) => Ok(given.then_some(report)),
                Err(()) => Err(Fault::new(code!("update.no_leave"), "x")),
            }
        }

        fn settings(&self) -> Settings {
            Settings::read(&settings::file(&self.dir)).unwrap()
        }

        /// Runs `command` at version 1.0.0 against `offered`, with a service when `service`
        /// says whether it comes up healthy.
        fn update(
            &self,
            command: UpdateCommand,
            offered: Result<ReleaseManifest, Fault>,
            service: Option<bool>,
        ) -> (Result<Ending, Fault>, String) {
            let offered = std::cell::RefCell::new(Some(offered));
            let release = || offered.borrow_mut().take().unwrap();
            let get = |_: &str| -> Result<fetch::Body, Fault> { Ok(Box::new(Cursor::new("new"))) };
            let version_of = |binary: &Path| Ok(says(binary, self.installed));
            let restart = || {
                self.restarts.set(self.restarts.get() + 1);
                let failed = Fault::new(code!("test.restart_failed"), "x");
                (!self.restart_fails).then_some(()).ok_or(failed)
            };
            let healthy = |_: &Commit| service.unwrap_or(false);
            let restart: &dyn Fn() -> Result<(), Fault> = &restart;
            let leave = || self.ask();
            let world = World {
                leave: &leave,
                release: &release,
                get: &get,
                target: LINUX,
                exe: self.dir.join("bin/weacon"),
                version_of: &version_of,
                restart: service.map(|_| restart),
                healthy: &healthy,
            };
            let mut out = Vec::new();
            let own = "1.0.0".parse().unwrap();
            let result = update(&self.dir, command, own, &world, &mut out);
            (result, String::from_utf8(out).unwrap())
        }
    }

    const NOW: UpdateCommand = UpdateCommand {
        yes: false,
        action: None,
    };
    const CHECK: UpdateCommand = UpdateCommand {
        yes: false,
        action: Some(UpdateAction::Check),
    };

    fn code(result: Result<Ending, Fault>) -> String {
        result.unwrap_err().code().as_str().to_owned()
    }

    #[test]
    fn a_newer_release_is_installed_and_the_service_restarted() {
        let machine = Machine::new();
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(result.unwrap(), Ending::Done);
        assert_eq!(out, "Installed weacon 1.2.3. The daemon runs it now.\n");
        assert_eq!(machine.binary(), "new");
        assert_eq!(machine.restarts.get(), 1);
        assert_eq!(machine.settings().last_published, 10);
        assert_eq!(machine.reports(), [InstallAction::Restarting]);
    }

    #[test]
    fn while_a_chat_is_busy_nothing_is_installed_and_the_schedule_tries_again() {
        let mut machine = Machine::new();
        machine.leave = Ok(false);
        let checked = Settings {
            last_checked: 99,
            ..Settings::default()
        };
        checked.write(&settings::file(&machine.dir)).unwrap();
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(result.unwrap(), Ending::Findings);
        assert_eq!(
            out,
            "A chat is busy, so weacon 1.2.3 wasn't installed. Run `weacon update` again once \
             its agent is done.\n"
        );
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.restarts.get(), 0);
        assert!(!machine.dir.join("bin/weacon.new").exists());
        let settings = machine.settings();
        assert_eq!((settings.last_checked, settings.last_published), (0, 10));
        assert!(machine.reports().is_empty());
    }

    #[test]
    fn a_daemon_that_gives_no_answer_gets_no_install() {
        let mut machine = Machine::new();
        machine.leave = Err(());
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(code(result), "update.no_leave");
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.restarts.get(), 0);
        assert!(!machine.dir.join("bin/weacon.new").exists());
    }

    #[test]
    fn leave_that_ran_out_leaves_the_daemon_running_on_the_old_build() {
        let mut machine = Machine::new();
        machine.takes = false;
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(result.unwrap(), Ending::Done);
        assert_eq!(
            out,
            "Installed weacon 1.2.3. Restart the daemon to run it.\n"
        );
        assert_eq!(machine.binary(), "new");
        assert_eq!(machine.restarts.get(), 0);
    }

    #[test]
    fn a_swap_that_fails_reports_the_failure() {
        let machine = Machine::new();
        fs::create_dir_all(machine.dir.join("bin/weacon.old/in")).unwrap();
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(code(result), "update.swap_failed");
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.reports(), [InstallAction::Failed]);
        assert_eq!(machine.restarts.get(), 0);
    }

    #[test]
    fn a_build_installed_meanwhile_from_another_state_folder_isnt_replaced() {
        let mut machine = Machine::new();
        machine.installed = "1.2.3";
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(code(result), "update.superseded");
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.restarts.get(), 0);
    }

    #[test]
    fn without_a_service_the_person_restarts_the_daemon() {
        let machine = Machine::new();
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), None);
        assert_eq!(result.unwrap(), Ending::Done);
        assert_eq!(
            out,
            "Installed weacon 1.2.3. Restart the daemon to run it.\n"
        );
        assert_eq!(machine.binary(), "new");
        assert_eq!(
            machine.asked.get(),
            0,
            "a daemon left running needs no leave"
        );
    }

    #[test]
    fn a_build_that_does_not_come_up_is_rolled_back() {
        let machine = Machine::new();
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(false));
        assert_eq!(code(result), "update.unhealthy");
        assert_eq!(out, "");
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.restarts.get(), 2);
        let reports = [InstallAction::Restarting, InstallAction::Failed];
        assert_eq!(machine.reports(), reports);
    }

    #[test]
    fn a_failed_restart_is_rolled_back_and_named_as_the_cause() {
        use std::error::Error as _;

        let mut machine = Machine::new();
        machine.restart_fails = true;
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        let fault = result.unwrap_err();
        assert_eq!(fault.code().as_str(), "update.unhealthy");
        let cause = fault
            .source()
            .and_then(|cause| cause.downcast_ref::<Fault>())
            .unwrap();
        assert_eq!(cause.code().as_str(), "test.restart_failed");
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.restarts.get(), 2);
    }

    #[test]
    fn the_same_or_an_older_version_is_up_to_date() {
        for version in ["1.0.0", "0.9.9"] {
            let machine = Machine::new();
            let (result, out) = machine.update(NOW, Ok(release(version, 10, PROTOCOL)), None);
            assert_eq!(result.unwrap(), Ending::Done);
            assert_eq!(out, "weacon is up to date.\n");
            assert_eq!(machine.binary(), "old");
            assert_eq!(machine.settings().last_published, 10);
        }
    }

    #[test]
    fn a_manifest_older_than_one_seen_is_refused() {
        let machine = Machine::new();
        let seen = Settings {
            last_published: 10,
            ..Settings::default()
        };
        seen.write(&settings::file(&machine.dir)).unwrap();
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 9, PROTOCOL)), None);
        assert_eq!(code(result), "update.stale");
        assert_eq!(machine.binary(), "old");
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), None);
        assert_eq!(result.unwrap(), Ending::Done);
    }

    #[test]
    fn a_check_only_says_what_is_out() {
        let machine = Machine::new();
        let (result, out) = machine.update(CHECK, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(result.unwrap(), Ending::Findings);
        assert_eq!(
            out,
            "weacon 1.2.3 is out. Run `weacon update` to install it.\n"
        );
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.restarts.get(), 0);
    }

    #[test]
    fn a_new_protocol_asks_first() {
        let machine = Machine::new();
        let newer = PROTOCOL + 1;
        let (result, out) = machine.update(NOW, Ok(release("2.0.0", 10, newer)), None);
        assert_eq!(result.unwrap(), Ending::Findings);
        assert!(out.contains("Run `weacon update --yes`"), "{out}");
        assert_eq!(machine.binary(), "old");
        let yes = UpdateCommand {
            yes: true,
            action: None,
        };
        let (result, _) = machine.update(yes, Ok(release("2.0.0", 10, newer)), None);
        assert_eq!(result.unwrap(), Ending::Done);
        assert_eq!(machine.binary(), "new");
    }

    #[test]
    fn a_release_without_this_machines_build_installs_nothing() {
        let machine = Machine::new();
        let mut other = release("1.2.3", 10, PROTOCOL);
        other.builds.clear();
        let (result, _) = machine.update(NOW, Ok(other), None);
        assert_eq!(code(result), "update.no_build");
        assert_eq!(machine.binary(), "old");
    }

    #[test]
    fn a_failed_fetch_changes_nothing() {
        let machine = Machine::new();
        let failed = Err(Fault::new(code!("update.fetch_failed"), "x"));
        let (result, _) = machine.update(NOW, failed, None);
        assert_eq!(code(result), "update.fetch_failed");
        assert_eq!(machine.settings(), Settings::default());
    }

    #[test]
    fn one_update_runs_at_a_time() {
        let machine = Machine::new();
        let _held = lock(&machine.dir).unwrap();
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), None);
        assert_eq!(code(result), "update.busy");
        assert_eq!(machine.binary(), "old");
    }

    #[test]
    fn a_schedule_is_kept_without_looking_for_a_release() {
        let machine = Machine::new();
        let weekly = UpdateCommand {
            yes: false,
            action: Some(UpdateAction::Schedule {
                schedule: UpdateSchedule::Weekly,
            }),
        };
        let failed = Err(Fault::new(code!("update.fetch_failed"), "x"));
        let (result, out) = machine.update(weekly, failed, None);
        assert_eq!(result.unwrap(), Ending::Done);
        assert_eq!(out, "Updates: install each week.\n");
        assert_eq!(machine.settings().schedule, UpdateSchedule::Weekly);
    }

    #[test]
    fn an_update_of_the_same_binary_from_another_state_folder_is_busy() {
        let machine = Machine::new();
        let paths = Paths::beside(&machine.dir.join("bin/weacon")).unwrap();
        let _held = held(&paths.lock, |_| panic!()).unwrap();
        let (result, _) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), None);
        assert_eq!(code(result), "update.busy");
        assert_eq!(machine.binary(), "old");
    }

    #[test]
    fn a_state_folder_without_room_for_the_lock_is_not_a_busy_one() {
        let dir = TempDir::new().unwrap();
        let fault = lock(&dir.join("missing")).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.state_unwritable");
    }

    #[test]
    fn a_broken_settings_file_can_still_be_turned_off() {
        let machine = Machine::new();
        let file = settings::file(&machine.dir);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not json").unwrap();
        let off = UpdateCommand {
            yes: false,
            action: Some(UpdateAction::Schedule {
                schedule: UpdateSchedule::Off,
            }),
        };
        let failed = || Err(Fault::new(code!("update.fetch_failed"), "x"));
        let (result, _) = machine.update(off, failed(), None);
        assert_eq!(result.unwrap(), Ending::Done);
        assert_eq!(machine.settings().schedule, UpdateSchedule::Off);
        let (result, _) = machine.update(NOW, failed(), None);
        assert_eq!(code(result), "update.fetch_failed");
    }

    #[test]
    fn status_mentions_a_newer_release_the_daemon_found() {
        let machine = Machine::new();
        assert_eq!(offer(&machine.dir, "1.0.0"), None);
        let found = Settings {
            offered: Some("1.2.3".parse().unwrap()),
            ..Settings::default()
        };
        found.write(&settings::file(&machine.dir)).unwrap();
        let line = "Update: weacon 1.2.3 is out. Run `weacon update` to install it.";
        assert_eq!(offer(&machine.dir, "1.0.0").as_deref(), Some(line));
        assert_eq!(offer(&machine.dir, "1.2.3"), None);
        assert_eq!(offer(&machine.dir, "dev"), None);
    }

    #[test]
    fn schedules_read_as_what_they_do() {
        let names = [
            UpdateSchedule::Off,
            UpdateSchedule::Notify,
            UpdateSchedule::Daily,
            UpdateSchedule::Weekly,
            UpdateSchedule::Newer,
        ]
        .map(schedule_name);
        assert_eq!(
            names,
            [
                "off",
                "say when a release is out",
                "install each day",
                "install each week",
                "say when a release is out"
            ]
        );
    }
}
