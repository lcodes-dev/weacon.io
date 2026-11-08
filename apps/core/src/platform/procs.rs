use std::io;

/// A running process: its id, its parent's, and its resident memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Process {
    pub(crate) pid: u32,
    pub(crate) parent: u32,
    pub(crate) rss_kib: u64,
}

/// Whether [`processes`] can list processes here.
pub(crate) const PROCESSES: bool = cfg!(target_os = "linux");

/// Every process this user can see, from `/proc`.
pub(crate) fn processes() -> io::Result<Vec<Process>> {
    if !PROCESSES {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok())
        else {
            continue;
        };
        // A process can exit between listing and reading; it's simply gone.
        let Ok(status) = std::fs::read_to_string(entry.path().join("status")) else {
            continue;
        };
        if let Some(parent) = field(&status, "PPid:") {
            found.push(Process {
                pid,
                parent: u32::try_from(parent).unwrap_or(0),
                // Kernel threads have no resident memory line.
                rss_kib: field(&status, "VmRSS:").unwrap_or(0),
            });
        }
    }
    Ok(found)
}

// The number after `name` in `/proc/<pid>/status`.
fn field(status: &str, name: &str) -> Option<u64> {
    status
        .lines()
        .find_map(|line| line.strip_prefix(name))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_read_from_a_status_file() {
        let status = "Name:\tclaude\nPPid:\t7\nVmRSS:\t   2048 kB\nThreads:\t9\n";

        assert_eq!(field(status, "PPid:"), Some(7));
        assert_eq!(field(status, "VmRSS:"), Some(2048));
        assert_eq!(field("Name:\tkthreadd\n", "VmRSS:"), None);
        assert_eq!(field("VmRSS:\t\n", "VmRSS:"), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_and_a_child_are_listed() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let listed = processes().unwrap();
        let own = std::process::id();
        child.kill().unwrap();
        child.wait().unwrap();

        let me = listed.iter().find(|process| process.pid == own).unwrap();
        assert!(me.rss_kib > 0);
        assert!(
            listed
                .iter()
                .any(|process| process.pid == child.id() && process.parent == own)
        );
    }
}
