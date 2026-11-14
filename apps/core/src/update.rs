//! `weacon update`: the core replaces its own binary with the latest release's build.

mod fetch;
#[cfg(test)]
mod fixtures;
mod install;
mod settings;
mod verify;
mod watch;

pub(crate) use watch::{CHECK_EVERY, Check, Launch, Watch};

use std::fs::{File, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use weacon_appkit::Outcome;
use weacon_observability::Fault;
use weacon_protocol::{
    BuildTarget, Commit, PROTOCOL, ReleaseManifest, ReleaseVersion, UpdateSchedule, code,
};

use crate::app::{App, state_unavailable};
use crate::cli::{UpdateAction, UpdateCommand};
use crate::platform::Env;
use crate::service;
use install::Paths;
use settings::Settings;

/// The target this binary was built for, as release manifests name it.
const TARGET: &str = env!("WEACON_TARGET");

/// How often, and how far apart, a restarted daemon is asked for its build: 30 s in all.
const HEALTH_TRIES: u32 = 60;
const HEALTH_PAUSE: Duration = Duration::from_millis(500);

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
}

/// Runs `weacon update` against the real world.
pub(crate) fn run(
    app: &App,
    command: UpdateCommand,
    env: Env<'_>,
    out: &mut impl Write,
) -> Result<Outcome, Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let exe = std::env::current_exe().map_err(|error| no_exe().because(error))?;
    let download = fetch::https(fetch::DOWNLOAD_WITHIN);
    let restart = || service::restart(&mut service::system);
    let healthy = |commit: &Commit| status_names(&exe, commit, HEALTH_TRIES, HEALTH_PAUSE);
    let restart: &dyn Fn() -> Result<(), Fault> = &restart;
    let world = World {
        release: &latest,
        get: &download,
        target: TARGET,
        exe: exe.clone(),
        version_of: &version_of,
        restart: service::installed(env).then_some(restart),
        healthy: &healthy,
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
) -> Result<Outcome, Fault> {
    let file = settings::file(state_dir);
    let _lock = lock(state_dir)?;
    if let Some(UpdateAction::Schedule { schedule }) = command.action {
        // A broken file mustn't keep its owner from turning updates off.
        let settings = Settings {
            schedule,
            ..Settings::read(&file).unwrap_or_default()
        };
        settings.write(&file)?;
        return say(out, &format!("Updates: {}.", schedule_name(schedule))).map(Outcome::from);
    }
    let mut settings = Settings::read(&file)?;
    let release = (world.release)()?;
    settings.accept(&release)?;
    settings.write(&file)?;
    let version = release.version;
    if version <= own {
        return say(out, "weacon is up to date.").map(Outcome::from);
    }
    if command.action == Some(UpdateAction::Check) {
        say(
            out,
            &format!("weacon {version} is out. Run `weacon update` to install it."),
        )?;
        return Ok(Outcome::Findings);
    }
    if release.protocol != PROTOCOL && !command.yes {
        say(
            out,
            &format!(
                "weacon {version} speaks a new protocol, so it may not reach a server that \
                 hasn't been updated. Run `weacon update --yes` to install it anyway."
            ),
        )?;
        return Ok(Outcome::Findings);
    }
    install_release(&release, world)?;
    let then = if world.restart.is_some() {
        "The daemon runs it now."
    } else {
        "Restart the daemon to run it."
    };
    say(out, &format!("Installed weacon {version}. {then}")).map(Outcome::from)
}

fn install_release(release: &ReleaseManifest, world: &World<'_>) -> Result<(), Fault> {
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
    install::stage(&paths, &mut (world.get)(build.url.as_str())?, &build)?;
    install::probe(&paths, &release.commit, (world.version_of)(&paths.new))?;
    install::swap(&paths)?;
    let Some(restart) = world.restart else {
        return Ok(());
    };
    if restart().is_ok() && (world.healthy)(&release.commit) {
        return Ok(());
    }
    let fault = install::roll_back(&paths);
    // The old build is back; starting it again is all that's left to try.
    drop(restart());
    Err(fault)
}

/// Holds the update lock until dropped; whoever comes second fails at once with `update.busy`.
fn lock(state_dir: &Path) -> Result<File, Fault> {
    let unwritable = |error| {
        Fault::new(
            code!("update.state_unwritable"),
            "The update lock couldn't be made in the state folder",
        )
        .because(error)
    };
    let file = File::create(state_dir.join("update.lock")).map_err(unwritable)?;
    file.try_lock().map_err(|error| match error {
        TryLockError::WouldBlock => Fault::new(code!("update.busy"), "Another update is running"),
        TryLockError::Error(error) => unwritable(error),
    })?;
    Ok(file)
}

fn version_of(binary: &Path) -> io::Result<String> {
    let output = Command::new(binary).arg("--version").output()?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

/// Asks `status` of the binary at `exe` up to `tries` times, `pause` apart, until it names
/// `commit`. The new binary asks, so it speaks the restarted daemon's protocol even when new.
fn status_names(exe: &Path, commit: &Commit, tries: u32, pause: Duration) -> bool {
    let wanted = format!("({})", commit.as_str());
    (0..tries).any(|_| {
        std::thread::sleep(pause);
        let status = Command::new(exe).arg("status").output();
        status.is_ok_and(|status| String::from_utf8_lossy(&status.stdout).contains(&wanted))
    })
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
                sha256: weacon_protocol::Sha256::try_from(format!("{:x}", Sha256::digest(body)))
                    .unwrap(),
            }],
        }
    }

    /// A state folder, and a running binary `bin/weacon` that holds `old`.
    struct Machine {
        dir: TempDir,
        restarts: Cell<u32>,
    }

    impl Machine {
        fn new() -> Self {
            let dir = TempDir::new().unwrap();
            fs::create_dir(dir.join("bin")).unwrap();
            fs::write(dir.join("bin/weacon"), "old").unwrap();
            Self {
                dir,
                restarts: Cell::new(0),
            }
        }

        fn binary(&self) -> String {
            fs::read_to_string(self.dir.join("bin/weacon")).unwrap()
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
        ) -> (Result<Outcome, Fault>, String) {
            let offered = std::cell::RefCell::new(Some(offered));
            let release = || offered.borrow_mut().take().unwrap();
            let get = |_: &str| -> Result<fetch::Body, Fault> { Ok(Box::new(Cursor::new("new"))) };
            let version_of = |_: &Path| Ok(format!("weacon 1.2.3 ({COMMIT})\n"));
            let restart = || {
                self.restarts.set(self.restarts.get() + 1);
                Ok(())
            };
            let healthy = |_: &Commit| service.unwrap_or(false);
            let restart: &dyn Fn() -> Result<(), Fault> = &restart;
            let world = World {
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

    fn code(result: Result<Outcome, Fault>) -> String {
        result.unwrap_err().code().as_str().to_owned()
    }

    #[test]
    fn a_newer_release_is_installed_and_the_service_restarted() {
        let machine = Machine::new();
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(result.unwrap(), Outcome::Done);
        assert_eq!(out, "Installed weacon 1.2.3. The daemon runs it now.\n");
        assert_eq!(machine.binary(), "new");
        assert_eq!(machine.restarts.get(), 1);
        assert_eq!(machine.settings().last_published, 10);
    }

    #[test]
    fn without_a_service_the_person_restarts_the_daemon() {
        let machine = Machine::new();
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), None);
        assert_eq!(result.unwrap(), Outcome::Done);
        assert_eq!(
            out,
            "Installed weacon 1.2.3. Restart the daemon to run it.\n"
        );
        assert_eq!(machine.binary(), "new");
    }

    #[test]
    fn a_build_that_does_not_come_up_is_rolled_back() {
        let machine = Machine::new();
        let (result, out) = machine.update(NOW, Ok(release("1.2.3", 10, PROTOCOL)), Some(false));
        assert_eq!(code(result), "update.unhealthy");
        assert_eq!(out, "");
        assert_eq!(machine.binary(), "old");
        assert_eq!(machine.restarts.get(), 2);
    }

    #[test]
    fn the_same_or_an_older_version_is_up_to_date() {
        for version in ["1.0.0", "0.9.9"] {
            let machine = Machine::new();
            let (result, out) = machine.update(NOW, Ok(release(version, 10, PROTOCOL)), None);
            assert_eq!(result.unwrap(), Outcome::Done);
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
        assert_eq!(result.unwrap(), Outcome::Done);
    }

    #[test]
    fn a_check_only_says_what_is_out() {
        let machine = Machine::new();
        let (result, out) = machine.update(CHECK, Ok(release("1.2.3", 10, PROTOCOL)), Some(true));
        assert_eq!(result.unwrap(), Outcome::Findings);
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
        assert_eq!(result.unwrap(), Outcome::Findings);
        assert!(out.contains("Run `weacon update --yes`"), "{out}");
        assert_eq!(machine.binary(), "old");
        let yes = UpdateCommand {
            yes: true,
            action: None,
        };
        let (result, _) = machine.update(yes, Ok(release("2.0.0", 10, newer)), None);
        assert_eq!(result.unwrap(), Outcome::Done);
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
        assert_eq!(result.unwrap(), Outcome::Done);
        assert_eq!(out, "Updates: install each week.\n");
        assert_eq!(machine.settings().schedule, UpdateSchedule::Weekly);
    }

    #[cfg(unix)]
    #[test]
    fn a_binary_is_asked_for_its_version() {
        let dir = TempDir::new().unwrap();
        let binary = dir.join("weacon");
        fs::write(&binary, "#!/bin/sh\necho \"asked $1\"\n").unwrap();
        crate::platform::make_runnable(&binary).unwrap();
        assert_eq!(version_of(&binary).unwrap(), "asked --version\n");
        version_of(&dir.join("missing")).unwrap_err();
    }

    /// A binary in `dir` that answers every command with `line`.
    #[cfg(unix)]
    fn answering(dir: &Path, line: &str) -> PathBuf {
        let binary = dir.join("weacon");
        fs::write(&binary, format!("#!/bin/sh\necho \"{line}\"\n")).unwrap();
        crate::platform::make_runnable(&binary).unwrap();
        binary
    }

    #[cfg(unix)]
    #[test]
    fn a_restarted_daemon_counts_once_its_status_names_the_new_commit() {
        let dir = TempDir::new().unwrap();
        let commit = Commit::try_from(COMMIT.to_owned()).unwrap();
        let new = answering(&dir, &format!("Daemon: running, weacon 1.2.3 ({COMMIT})"));
        assert!(status_names(&new, &commit, 1, Duration::ZERO));
        assert!(!status_names(&new, &commit, 0, Duration::ZERO));
        let old = answering(&dir, "Daemon: running, weacon 1.0.0 (1a2b3c4)");
        assert!(!status_names(&old, &commit, 3, Duration::ZERO));
        assert!(!status_names(
            &dir.join("missing"),
            &commit,
            1,
            Duration::ZERO
        ));
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
        assert_eq!(result.unwrap(), Outcome::Done);
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
