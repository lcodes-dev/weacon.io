//! Runs the release script's manifest step, so a manifest it writes is one every core reads.

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use std::path::Path;
    use std::process::Command;
    use std::{env, fs};

    use weacon_protocol::{BuildTarget, ReleaseManifest};
    use weacon_testing::TempDir;

    const TARGETS: [&str; 5] = [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ];

    fn file_name(target: &str) -> String {
        let exe = if target.contains("windows") {
            ".exe"
        } else {
            ""
        };
        format!("weacon-{target}{exe}")
    }

    // Fixed, so the test reads the same in a copy of the tree with no git.
    const COMMIT: &str = "0123456789abcdef0123456789abcdef01234567";

    fn manifest(dir: &Path, commit: &str) -> std::process::Output {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let tag = format!("v{}", env!("CARGO_PKG_VERSION"));
        let base = format!("https://github.com/o/r/releases/download/{tag}");
        Command::new("bash")
            .arg(root.join("scripts/release.sh"))
            .args(["manifest", &tag, dir.to_str().unwrap(), &base])
            .env("WEACON_COMMIT", commit)
            .output()
            .unwrap()
    }

    #[test]
    fn the_release_manifest_is_one_every_core_reads() {
        let dir = TempDir::new().unwrap();
        for target in TARGETS {
            fs::write(dir.join(file_name(target)), target).unwrap();
        }
        fs::write(
            dir.join("weacon-server-x86_64-unknown-linux-musl"),
            "server",
        )
        .unwrap();
        let output = manifest(&dir, COMMIT);
        assert!(output.status.success(), "{output:?}");

        let json = fs::read(dir.join("manifest.json")).unwrap();
        let manifest: ReleaseManifest = serde_json::from_slice(&json).unwrap();
        assert_eq!(manifest.version.to_string(), env!("CARGO_PKG_VERSION"));
        assert_eq!(manifest.published_at, 0);
        assert_eq!(manifest.commit.as_str(), COMMIT);
        assert_eq!(manifest.builds.len(), TARGETS.len());
        for target in TARGETS {
            let build = manifest.build_for(&BuildTarget::try_from(target.to_owned()).unwrap());
            let build = build.unwrap();
            assert_eq!(build.size, u64::try_from(target.len()).unwrap());
            assert!(
                build
                    .url
                    .as_str()
                    .ends_with(&format!("/{}", file_name(target)))
            );
        }
    }

    #[test]
    fn a_short_commit_has_no_manifest() {
        let dir = TempDir::new().unwrap();
        for target in TARGETS {
            fs::write(dir.join(file_name(target)), target).unwrap();
        }
        let output = manifest(&dir, "0123456");
        assert!(!output.status.success());
        assert!(!dir.join("manifest.json").exists());
    }

    #[test]
    fn a_release_missing_a_build_has_no_manifest() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("weacon-x86_64-unknown-linux-musl"), "linux").unwrap();
        let output = manifest(&dir, COMMIT);
        assert!(!output.status.success());
        assert!(!dir.join("manifest.json").exists());
    }
}
