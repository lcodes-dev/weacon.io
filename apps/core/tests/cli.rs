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
        let file = temp_dir("doctor-file");
        fs::write(&file, "").unwrap();
        let good = weacon(&["doctor"], dir.to_str().unwrap());
        // A folder can't be made inside a file.
        let blocked = weacon(&["doctor"], file.join("state").to_str().unwrap());

        assert!(good.status.success());
        assert!(dir.join("logs/cli.log").exists());
        assert_eq!(blocked.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&blocked.stderr).contains("state.unavailable"));
        fs::remove_dir_all(&dir).unwrap();
        fs::remove_file(&file).unwrap();
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
    fn the_daemon_serves_status() {
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

        assert!(status.status.success());
        assert!(
            String::from_utf8(status.stdout)
                .unwrap()
                .starts_with("Daemon: running, weacon ")
        );
        stop(daemon, &dir);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    fn stop(mut daemon: std::process::Child, dir: &std::path::Path) {
        let term = Command::new("kill")
            .args(["-TERM", &daemon.id().to_string()])
            .status()
            .unwrap();

        assert!(term.success());
        assert!(daemon.wait().unwrap().success());
        assert!(!dir.join("weacon.sock").exists());
    }

    // Windows has no SIGTERM to send another process; the daemon is just ended.
    #[cfg(windows)]
    fn stop(mut daemon: std::process::Child, _: &std::path::Path) {
        daemon.kill().unwrap();
        daemon.wait().unwrap();
    }

    // Chrome's frames: a native-endian length, then that many bytes of JSON.
    fn frame(json: &str) -> Vec<u8> {
        let mut frame = u32::try_from(json.len()).unwrap().to_ne_bytes().to_vec();
        frame.extend(json.as_bytes());
        frame
    }

    #[test]
    fn the_native_host_relays_frames_and_ends_with_the_daemon() {
        use std::io::{Read, Write};

        let dir = temp_dir("native-host");
        let mut daemon = Command::new(env!("CARGO_BIN_EXE_weacon"))
            .args(["daemon", "--verbose"])
            .env("WEACON_STATE_DIR", &dir)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(daemon.stderr.take().unwrap()).lines();
        while !stderr.next().unwrap().unwrap().contains("daemon started") {}
        let mut host = Command::new(env!("CARGO_BIN_EXE_weacon"))
            .arg("chrome-extension://phnfnmnpncfhkhlkmglnlhlkaaociaij/")
            .env("WEACON_STATE_DIR", &dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut stdin = host.stdin.take().unwrap();
        let mut stdout = host.stdout.take().unwrap();
        let hello = r#"{"id":1,"type":"hello","protocol":1,"app":{"name":"weacon-extension","version":"0.1.0","commit":"1a2b3c4"}}"#;
        stdin.write_all(&frame(hello)).unwrap();
        let mut length = [0; 4];
        stdout.read_exact(&mut length).unwrap();
        let mut reply = vec![0; usize::try_from(u32::from_ne_bytes(length)).unwrap()];
        stdout.read_exact(&mut reply).unwrap();

        assert!(
            String::from_utf8(reply)
                .unwrap()
                .contains(r#""type":"welcome""#)
        );
        stop(daemon, &dir);
        // Chrome still holds stdin open; the host must notice the daemon is gone anyway.
        let (done, exited) = std::sync::mpsc::channel();
        std::thread::spawn(move || done.send(host.wait()).unwrap());
        let status = exited.recv_timeout(std::time::Duration::from_secs(20));
        assert!(status.is_ok(), "the host outlived the daemon");
        drop(stdin);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn connect_chrome_registers_this_program() {
        let dir = temp_dir("connect-chrome");
        let output = Command::new(env!("CARGO_BIN_EXE_weacon"))
            .arg("connect-chrome")
            .env("WEACON_STATE_DIR", dir.join("state"))
            .env("HOME", &dir)
            .env_remove("XDG_CONFIG_HOME")
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let manifest = stdout
            .lines()
            .find_map(|line| line.strip_prefix("Registered: "))
            .unwrap();

        assert!(output.status.success());
        assert!(
            fs::read_to_string(manifest)
                .unwrap()
                .contains(env!("CARGO_BIN_EXE_weacon"))
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_bad_command_line_exits_with_usage_status() {
        assert_eq!(weacon(&["fly"], "/unused").status.code(), Some(2));
    }

    // Nowhere to put the unit, so it fails before touching systemd.
    #[cfg(target_os = "linux")]
    #[test]
    fn service_install_needs_a_home() {
        let output = Command::new(env!("CARGO_BIN_EXE_weacon"))
            .args(["service", "install"])
            .env_clear()
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&output.stderr).contains("service.no_config_dir"));
    }
}
