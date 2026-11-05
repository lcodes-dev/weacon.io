//! Runs the built `weacon` binary.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;
    use std::process::{Command, Output, Stdio};
    use std::{env, fs};

    fn weacon(args: &[&str], state_dir: &str) -> Output {
        Command::new(env!("CARGO_BIN_EXE_weacon"))
            .args(args)
            .env("WEACON_STATE_DIR", state_dir)
            .env_remove("WEACON_SENTRY_DSN")
            .output()
            .unwrap()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = env::temp_dir().join(format!("weacon-core-cli-{name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        dir
    }

    #[test]
    fn version_prints_the_version_and_commit() {
        let output = weacon(&["--version"], "/unused");
        let text = String::from_utf8(output.stdout).unwrap();

        assert!(output.status.success());
        assert!(
            text.starts_with(&format!("weacon {} (", env!("CARGO_PKG_VERSION"))),
            "{text}"
        );
    }

    #[test]
    fn doctor_succeeds_with_a_state_folder_and_fails_without_one() {
        let dir = temp_dir("doctor");
        let good = weacon(&["doctor"], dir.to_str().unwrap());
        let blocked = weacon(&["doctor"], "/proc/weacon-cannot-be-here");

        assert!(good.status.success());
        assert!(dir.join("logs/cli.log").exists());
        assert_eq!(blocked.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("state.unavailable"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn status_says_when_no_daemon_is_running() {
        let dir = temp_dir("status");
        let output = weacon(&["status"], dir.to_str().unwrap());

        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("daemon.not_running"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_daemon_serves_status_and_stops_on_sigterm() {
        let dir = temp_dir("daemon");
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_weacon"))
            .args(["daemon", "--verbose"])
            .env("WEACON_STATE_DIR", &dir)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(daemon.stderr.take().unwrap()).lines();
        while !stderr.next().unwrap().unwrap().contains("daemon started") {}
        let status = weacon(&["status"], dir.to_str().unwrap());
        let term = Command::new("kill")
            .args(["-TERM", &daemon.id().to_string()])
            .status()
            .unwrap();
        let exit = daemon.wait().unwrap();

        assert!(status.status.success());
        assert!(
            String::from_utf8(status.stdout)
                .unwrap()
                .starts_with("Daemon: running, weacon ")
        );
        assert!(term.success());
        assert!(exit.success());
        assert!(!dir.join("weacon.sock").exists());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bad_command_line_exits_with_usage_status() {
        assert_eq!(weacon(&["fly"], "/unused").status.code(), Some(2));
    }
}
