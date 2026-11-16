//! Keeps the release script's targets and the release workflow's in step.

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    const TARGETS: [&str; 5] = [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ];

    // The release workflow can't read the script's lists, so its matrices repeat them.
    #[test]
    fn the_script_and_the_release_workflow_build_the_same_targets() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let script = "eval \"$(sed -n '/^LINUX_TARGETS=/,/^CORE_TARGETS=/p' scripts/release.sh)\"
            printf '%s ' \"${CORE_TARGETS[@]}\"; echo; printf '%s ' \"${LINUX_TARGETS[@]}\"";
        let output = Command::new("bash")
            .args(["-c", script])
            .current_dir(&root)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let listed = String::from_utf8(output.stdout).unwrap();
        let lists: Vec<Vec<_>> = listed
            .lines()
            .map(|line| line.split_whitespace().collect())
            .collect();
        assert_eq!(lists, [&TARGETS[..], &TARGETS[..2]]);

        let workflow = fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap();
        let (core, server) = workflow.split_once("\n  server:").unwrap();
        assert_eq!(matrix(core), lists[0]);
        assert_eq!(matrix(server), lists[1]);
    }

    fn matrix(job: &str) -> Vec<&str> {
        job.lines()
            .filter_map(|line| line.trim().strip_prefix("- { target: "))
            .filter_map(|entry| entry.split(',').next())
            .collect()
    }
}
