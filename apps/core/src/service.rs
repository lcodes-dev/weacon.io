use std::path::PathBuf;
use std::process::Command;

use crate::{fail, printed};

const NAME: &str = "io.weacon.core";

fn kept() -> Vec<(String, String)> {
    let names = [
        "PATH",
        "WEACON_CORE_HOME",
        "CLAUDE_CONFIG_DIR",
        "CODEX_HOME",
    ];
    let set = names
        .iter()
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)));
    set.collect()
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn ran(program: &str, args: &[&str]) -> bool {
    let ended = Command::new(program).args(args).status();
    ended.is_ok_and(|ended| ended.success())
}

fn tried(program: &str, args: &[&str]) {
    let quiet = std::process::Stdio::null();
    Command::new(program).args(args).stderr(quiet).status().ok();
}

fn written(file: &PathBuf, text: &str) {
    let folder = file.parent().expect("a file is in a folder");
    let kept = std::fs::create_dir_all(folder).and_then(|()| std::fs::write(file, text));
    if let Err(error) = kept {
        fail(format!("{} can't be written: {error}", file.display()));
    }
}

fn xml(value: &str) -> String {
    let value = value.replace('&', "&amp;").replace('<', "&lt;");
    value.replace('>', "&gt;")
}

fn quoted(value: &str) -> String {
    let value = value.replace('\\', "\\\\").replace('"', "\\\"");
    value.replace('%', "%%")
}

fn launch_agent() -> PathBuf {
    home().join(format!("Library/LaunchAgents/{NAME}.plist"))
}

fn user_service() -> PathBuf {
    home().join(".config/systemd/user/weacon-core.service")
}

pub fn install() {
    let core = std::env::current_exe().unwrap_or_else(|error| fail(error));
    let core = core.to_string_lossy();
    if cfg!(target_os = "macos") {
        let variables: String = kept()
            .iter()
            .map(|(name, value)| format!("<key>{name}</key><string>{}</string>", xml(value)))
            .collect();
        let log = home().join("Library/Logs/weacon-core.log");
        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{NAME}</string>
<key>ProgramArguments</key><array><string>{program}</string></array>
<key>EnvironmentVariables</key><dict>{variables}</dict>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><true/>
<key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#,
            program = xml(&core),
            log = xml(&log.to_string_lossy())
        );
        let file = launch_agent();
        written(&file, &plist);
        let user = format!("gui/{}", printed("id", &["-u"]).unwrap_or_default());
        tried("launchctl", &["bootout", &format!("{user}/{NAME}")]);
        if !ran("launchctl", &["bootstrap", &user, &file.to_string_lossy()]) {
            fail("The launch agent was written but couldn't be started.");
        }
        println!(
            "weacon-core now starts when you log in and runs in the background. It logs to {}.",
            log.display()
        );
        return;
    }
    let variables: String = kept()
        .iter()
        .map(|(name, value)| format!("Environment=\"{name}={}\"\n", quoted(value)))
        .collect();
    let unit = format!(
        "[Unit]\nDescription=Weacon core\nAfter=network-online.target\n\n[Service]\nExecStart=\"{}\"\nRestart=always\nRestartSec=5\n{variables}\n[Install]\nWantedBy=default.target\n",
        quoted(&core)
    );
    written(&user_service(), &unit);
    if !ran("systemctl", &["--user", "daemon-reload"]) {
        fail(
            "The service was written, but this system has no systemd for your user.\nIn WSL, add these two lines to /etc/wsl.conf, then run `wsl --shutdown` in Windows and try again:\n  [boot]\n  systemd=true\nUntil then, run `weacon-core` in a terminal to connect.",
        );
    }
    if !ran("systemctl", &["--user", "enable", "--now", "weacon-core"]) {
        fail(
            "The service was written but couldn't be started. `systemctl --user status weacon-core` says why.",
        );
    }
    let user = std::env::var("USER").unwrap_or_default();
    if !ran("loginctl", &["enable-linger", &user]) {
        println!(
            "It runs while you are logged in. `loginctl enable-linger {user}` keeps it running after you log out."
        );
    }
    println!(
        "weacon-core now starts with this computer and runs in the background. `journalctl --user -u weacon-core` shows its log."
    );
}

pub fn uninstall() {
    if cfg!(target_os = "macos") {
        let user = format!("gui/{}", printed("id", &["-u"]).unwrap_or_default());
        tried("launchctl", &["bootout", &format!("{user}/{NAME}")]);
        std::fs::remove_file(launch_agent()).ok();
    } else {
        tried("systemctl", &["--user", "disable", "--now", "weacon-core"]);
        std::fs::remove_file(user_service()).ok();
        tried("systemctl", &["--user", "daemon-reload"]);
    }
    println!("weacon-core no longer runs in the background. This machine is still paired.");
}

#[cfg(test)]
mod tests {
    use super::{quoted, xml};

    #[test]
    fn values_are_written_the_way_each_file_asks() {
        assert_eq!(
            xml("/Volumes/R&D <x>/bin"),
            "/Volumes/R&amp;D &lt;x&gt;/bin"
        );
        assert_eq!(quoted(r#"/home/a b/50%"c\d"#), r#"/home/a b/50%%\"c\\d"#);
    }
}
