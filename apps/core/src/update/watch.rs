//! The daemon's schedule: it looks for a release when its owner's schedule says, and on `daily`
//! or `weekly` starts `weacon update` to install it. On `off` it never asks anyone.

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::task::JoinError;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{PROTOCOL, ReleaseManifest, ReleaseVersion, UpdateSchedule, code};

use super::settings::{self, Settings};

const DAY: u64 = 24 * 60 * 60;
const WEEK: u64 = 7 * DAY;

/// How often the daemon asks itself whether a check is due.
pub(crate) const CHECK_EVERY: Duration = Duration::from_hours(1);

/// The latest release, once its signature checks out.
pub(crate) type Latest = fn() -> Result<ReleaseManifest, Fault>;
/// Starts `weacon update` apart from the daemon for a state folder; false where it can't.
pub(crate) type Launch = fn(&Path) -> io::Result<bool>;
/// Seconds since the Unix epoch.
pub(crate) type Now = fn() -> u64;

pub(crate) struct Watch {
    state_dir: PathBuf,
    own: ReleaseVersion,
    latest: Latest,
    launch: Launch,
    now: Now,
    failing: bool,
}

/// A check running off the async runtime, since it waits on the network.
pub(crate) type Check = tokio::task::JoinHandle<Result<Found, Fault>>;

/// What one check came to.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Found {
    Nothing,
    Offered(ReleaseVersion),
    Started(ReleaseVersion),
}

impl Watch {
    pub(crate) const fn new(
        state_dir: PathBuf,
        own: ReleaseVersion,
        latest: Latest,
        launch: Launch,
        now: Now,
    ) -> Self {
        Self {
            state_dir,
            own,
            latest,
            launch,
            now,
            failing: false,
        }
    }

    /// Starts a check, which does nothing unless one is due.
    pub(crate) fn start(&self) -> Check {
        let (dir, own, latest, launch, now) = (
            self.state_dir.clone(),
            self.own,
            self.latest,
            self.launch,
            self.now,
        );
        tokio::task::spawn_blocking(move || step(&dir, own, latest, launch, now()))
    }

    /// Logs what a finished check found.
    pub(crate) fn finished(
        &mut self,
        joined: Result<Result<Found, Fault>, JoinError>,
        log: &Logger,
    ) {
        let found = joined.unwrap_or_else(|_| {
            Err(Fault::new(
                code!("update.check_failed"),
                "The update check stopped",
            ))
        });
        self.observe(found, log);
    }

    /// A failed check is logged once until one succeeds.
    fn observe(&mut self, found: Result<Found, Fault>, log: &Logger) {
        match found {
            Ok(Found::Nothing) => {}
            Ok(Found::Offered(version)) => {
                log.info(
                    "update available",
                    &[("version", version.to_string().into())],
                );
            }
            Ok(Found::Started(version)) => {
                log.info("update started", &[("version", version.to_string().into())]);
            }
            Err(error) => {
                if !self.failing {
                    log.warn("update check failed", &[("err", Field::error(&error))]);
                }
                self.failing = true;
                return;
            }
        }
        self.failing = false;
    }
}

fn step(
    state_dir: &Path,
    own: ReleaseVersion,
    latest: Latest,
    launch: Launch,
    now: u64,
) -> Result<Found, Fault> {
    let file = settings::file(state_dir);
    if !due(Settings::read(&file)?, now) {
        return Ok(Found::Nothing);
    }
    record(state_dir, own, &latest()?, launch, now)
}

/// Records what a check fetched, and starts the install it calls for.
fn record(
    state_dir: &Path,
    own: ReleaseVersion,
    release: &ReleaseManifest,
    launch: Launch,
    now: u64,
) -> Result<Found, Fault> {
    let file = settings::file(state_dir);
    let lock = match super::lock(state_dir) {
        Err(fault) if fault.code().as_str() == "update.busy" => return Ok(Found::Nothing),
        lock => lock?,
    };
    // Read again, so a choice its owner made during the fetch stands.
    let mut settings = Settings::read(&file)?;
    if settings.schedule.effective() == UpdateSchedule::Off {
        return Ok(Found::Nothing);
    }
    settings.last_checked = now;
    let accepted = settings.accept(release);
    let newer = accepted.is_ok() && release.version > own;
    settings.offered = newer.then_some(release.version);
    settings.write(&file)?;
    drop(lock);
    accepted?;
    if !newer {
        return Ok(Found::Nothing);
    }
    let installs = matches!(
        settings.schedule.effective(),
        UpdateSchedule::Daily | UpdateSchedule::Weekly
    );
    // A new protocol always waits for its owner to say yes.
    if installs && release.protocol == PROTOCOL && launched(launch, state_dir)? {
        return Ok(Found::Started(release.version));
    }
    Ok(Found::Offered(release.version))
}

/// Whether `settings` call for a check at `now`; a check stamped in the future counts as due.
fn due(settings: Settings, now: u64) -> bool {
    let every = match settings.schedule.effective() {
        UpdateSchedule::Off => return false,
        UpdateSchedule::Weekly => WEEK,
        UpdateSchedule::Notify | UpdateSchedule::Daily | UpdateSchedule::Newer => DAY,
    };
    !(settings.last_checked..settings.last_checked.saturating_add(every)).contains(&now)
}

fn launched(launch: Launch, state_dir: &Path) -> Result<bool, Fault> {
    launch(state_dir).map_err(|error| {
        Fault::new(
            code!("update.launch_failed"),
            "The update couldn't be started",
        )
        .because(error)
    })
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{BuildTarget, Commit, DownloadUrl, ReleaseBuild, Sha256};
    use weacon_testing::{TempDir, messages, recording_logger};

    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn release(version: &str, protocol: u32) -> ReleaseManifest {
        ReleaseManifest {
            version: version.parse().unwrap(),
            commit: Commit::try_from("0".repeat(40)).unwrap(),
            published_at: 5,
            protocol,
            builds: vec![ReleaseBuild {
                target: BuildTarget::try_from("x".to_owned()).unwrap(),
                url: DownloadUrl::try_from("https://example.com/w".to_owned()).unwrap(),
                size: 1,
                sha256: Sha256::try_from("0".repeat(64)).unwrap(),
            }],
        }
    }

    fn with(dir: &Path, schedule: UpdateSchedule, last_checked: u64) {
        let settings = Settings {
            schedule,
            last_checked,
            ..Settings::default()
        };
        settings.write(&settings::file(dir)).unwrap();
    }

    const NEWER: Latest = || Ok(release("2.0.0", PROTOCOL));
    const SAME: Latest = || Ok(release("1.0.0", PROTOCOL));
    const NEW_PROTOCOL: Latest = || Ok(release("2.0.0", PROTOCOL + 1));
    const UNREACHABLE: Latest = || Err(Fault::new(code!("update.fetch_failed"), "x"));
    const MUST_NOT_ASK: Latest = || Err(Fault::new(code!("test.asked"), "x"));
    const STARTS: Launch = |_| Ok(true);
    const CANNOT_START: Launch = |_| Ok(false);
    const FAILS_TO_START: Launch = |_| Err(io::Error::from(io::ErrorKind::NotFound));

    fn run(dir: &Path, latest: Latest, launch: Launch) -> Result<Found, Fault> {
        step(dir, "1.0.0".parse().unwrap(), latest, launch, NOW)
    }

    fn version(text: &str) -> ReleaseVersion {
        text.parse().unwrap()
    }

    #[test]
    fn off_never_asks() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Off, 0);
        assert_eq!(run(&dir, MUST_NOT_ASK, STARTS).unwrap(), Found::Nothing);
    }

    #[test]
    fn notify_says_what_is_out_and_installs_nothing() {
        let dir = TempDir::new().unwrap();
        assert_eq!(
            run(&dir, NEWER, STARTS).unwrap(),
            Found::Offered(version("2.0.0"))
        );
        assert_eq!(
            Settings::read(&settings::file(&dir)).unwrap().last_checked,
            NOW
        );
    }

    #[test]
    fn daily_and_weekly_start_the_update() {
        for schedule in [UpdateSchedule::Daily, UpdateSchedule::Weekly] {
            let dir = TempDir::new().unwrap();
            with(&dir, schedule, 0);
            assert_eq!(
                run(&dir, NEWER, STARTS).unwrap(),
                Found::Started(version("2.0.0"))
            );
        }
    }

    #[test]
    fn without_a_way_to_start_it_the_update_is_only_offered() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Daily, 0);
        assert_eq!(
            run(&dir, NEWER, CANNOT_START).unwrap(),
            Found::Offered(version("2.0.0"))
        );
        with(&dir, UpdateSchedule::Daily, 0);
        let fault = run(&dir, NEWER, FAILS_TO_START).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.launch_failed");
    }

    #[test]
    fn a_new_protocol_waits_for_its_owner() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Daily, 0);
        assert_eq!(
            run(&dir, NEW_PROTOCOL, STARTS).unwrap(),
            Found::Offered(version("2.0.0"))
        );
    }

    #[test]
    fn nothing_newer_is_nothing() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Daily, 0);
        assert_eq!(run(&dir, SAME, STARTS).unwrap(), Found::Nothing);
        let seen = Settings {
            last_published: 6,
            ..Settings::default()
        };
        seen.write(&settings::file(&dir)).unwrap();
        let fault = run(&dir, NEWER, STARTS).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.stale");
        let settings = Settings::read(&settings::file(&dir)).unwrap();
        assert_eq!((settings.last_checked, settings.offered), (NOW, None));
    }

    #[test]
    fn checks_wait_a_day_or_a_week() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Notify, NOW - 86_399);
        assert_eq!(run(&dir, MUST_NOT_ASK, STARTS).unwrap(), Found::Nothing);
        with(&dir, UpdateSchedule::Notify, NOW - 86_400);
        assert_eq!(
            run(&dir, NEWER, STARTS).unwrap(),
            Found::Offered(version("2.0.0"))
        );
        with(&dir, UpdateSchedule::Weekly, NOW - 604_799);
        assert_eq!(run(&dir, MUST_NOT_ASK, STARTS).unwrap(), Found::Nothing);
        with(&dir, UpdateSchedule::Weekly, NOW - 604_800);
        assert_eq!(
            run(&dir, NEWER, STARTS).unwrap(),
            Found::Started(version("2.0.0"))
        );
    }

    #[test]
    fn a_check_stamped_in_the_future_is_due() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Notify, NOW + 1);
        assert_eq!(
            run(&dir, NEWER, STARTS).unwrap(),
            Found::Offered(version("2.0.0"))
        );
    }

    #[test]
    fn turning_updates_off_during_a_fetch_stands() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Off, 3);
        let found = record(
            &dir,
            version("1.0.0"),
            &release("2.0.0", PROTOCOL),
            STARTS,
            NOW,
        );
        assert_eq!(found.unwrap(), Found::Nothing);
        let settings = Settings::read(&settings::file(&dir)).unwrap();
        assert_eq!(
            (settings.schedule, settings.last_checked),
            (UpdateSchedule::Off, 3)
        );
    }

    #[test]
    fn a_state_folder_without_room_for_the_lock_is_an_error() {
        let dir = TempDir::new().unwrap();
        let missing = dir.join("missing");
        let found = record(
            &missing,
            version("1.0.0"),
            &release("2.0.0", PROTOCOL),
            STARTS,
            NOW,
        );
        assert_eq!(
            found.unwrap_err().code().as_str(),
            "update.state_unwritable"
        );
    }

    #[test]
    fn a_check_skips_while_an_update_holds_the_lock() {
        let dir = TempDir::new().unwrap();
        with(&dir, UpdateSchedule::Daily, 0);
        let _held = super::super::lock(&dir).unwrap();
        let found = record(
            &dir,
            version("1.0.0"),
            &release("2.0.0", PROTOCOL),
            STARTS,
            NOW,
        );
        assert_eq!(found.unwrap(), Found::Nothing);
        assert_eq!(
            Settings::read(&settings::file(&dir)).unwrap().last_checked,
            0
        );
    }

    #[test]
    fn a_failed_check_warns_once_and_tries_again_next_tick() {
        let dir = TempDir::new().unwrap();
        let fault = run(&dir, UNREACHABLE, STARTS).unwrap_err();
        assert_eq!(fault.code().as_str(), "update.fetch_failed");
        assert_eq!(
            Settings::read(&settings::file(&dir)).unwrap().last_checked,
            0
        );

        let (log, records) = recording_logger();
        let mut watch = Watch::new(dir.to_path_buf(), version("1.0.0"), NEWER, STARTS, || NOW);
        watch.observe(Err(fault), &log);
        watch.observe(run(&dir, UNREACHABLE, STARTS), &log);
        watch.observe(Ok(Found::Offered(version("2.0.0"))), &log);
        watch.observe(run(&dir, UNREACHABLE, STARTS), &log);
        watch.observe(Ok(Found::Started(version("2.0.0"))), &log);
        assert_eq!(
            messages(&records),
            [
                "update check failed",
                "update available",
                "update check failed",
                "update started"
            ]
        );
    }

    #[tokio::test]
    async fn a_check_runs_off_the_runtime() {
        let dir = TempDir::new().unwrap();
        let (log, records) = recording_logger();
        let mut watch = Watch::new(dir.to_path_buf(), version("1.0.0"), NEWER, STARTS, || NOW);
        let joined = watch.start().await;
        watch.finished(joined, &log);
        assert_eq!(messages(&records), ["update available"]);
    }
}
