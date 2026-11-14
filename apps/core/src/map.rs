use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;

use weacon_appkit::Outcome;
use weacon_codemap::{SourceFile, index};
use weacon_observability::Fault;
use weacon_protocol::code;

use crate::app::App;
use crate::cli::MapCommand;

/// Checks the repository; `Findings` when a pin no longer matches.
pub(crate) fn run(
    app: &App,
    command: MapCommand,
    input: &mut impl Read,
    out: &mut impl Write,
) -> Result<Outcome, Fault> {
    let MapCommand::CheckAnchors { root } = command;
    let clean = check_anchors(app, &root, input, out)?;
    Ok(if clean {
        Outcome::Done
    } else {
        Outcome::Findings
    })
}

/// Prints each pin read from `input` that no longer matches the repository at `root`, and
/// says whether every pin matches.
fn check_anchors(
    app: &App,
    root: &Path,
    input: &mut impl Read,
    out: &mut impl Write,
) -> Result<bool, Fault> {
    let mut pins = String::new();
    input.read_to_string(&mut pins).map_err(|error| {
        Fault::new(code!("map.input_failed"), "The anchors couldn't be read").because(error)
    })?;
    let started = Instant::now();
    let mut files = Vec::new();
    source_files(root, "", &mut files).map_err(|error| {
        Fault::new(code!("map.read_failed"), "The repository couldn't be read").because(error)
    })?;
    let index = index(&files).map_err(|error| {
        Fault::new(code!("map.index_failed"), "The code map couldn't be built").because(error)
    })?;
    let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    app.log.info(
        "map indexed",
        &[("files", files.len().into()), ("ms", millis.into())],
    );
    let findings = index.check(&pins);
    let report: String = findings
        .iter()
        .map(|finding| finding.to_string() + "\n")
        .collect();
    out.write_all(report.as_bytes()).map_err(|error| {
        Fault::new(
            code!("map.output_failed"),
            "The findings couldn't be printed",
        )
        .because(error)
    })?;
    Ok(findings.is_empty())
}

/// Every `Cargo.toml`, and every `.rs` file inside a `src` folder, under `root/folder`, its path
/// joined with `/`. Hidden entries, links, `node_modules`, Cargo's build folders and files
/// that aren't UTF-8 are left out.
fn source_files(root: &Path, folder: &str, files: &mut Vec<SourceFile>) -> io::Result<()> {
    for entry in fs::read_dir(root.join(folder))? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| !name.starts_with('.')) else {
            continue;
        };
        let path = if folder.is_empty() {
            name.to_owned()
        } else {
            format!("{folder}/{name}")
        };
        let kind = entry.file_type()?;
        if kind.is_dir() && !skipped(&root.join(&path), name) {
            source_files(root, &path, files)?;
        } else if kind.is_file()
            && is_source(&path, name)
            && let Some(text) = text_or_skip(fs::read_to_string(root.join(&path)))?
        {
            files.push(SourceFile { path, text });
        }
    }
    Ok(())
}

/// A file's text, or `None` for a file that isn't UTF-8.
fn text_or_skip(read: io::Result<String>) -> io::Result<Option<String>> {
    match read {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => Ok(None),
        Err(error) => Err(error),
    }
}

/// `node_modules`, and folders Cargo marks as its build output.
fn skipped(folder: &Path, name: &str) -> bool {
    name == "node_modules" || folder.join("CACHEDIR.TAG").is_file()
}

fn is_source(path: &str, name: &str) -> bool {
    let rust = Path::new(name).extension().is_some_and(|it| it == "rs");
    name == "Cargo.toml" || (rust && path.split('/').any(|part| part == "src"))
}

#[cfg(test)]
mod tests {
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{Broken, app};

    fn repo(files: &[(&str, &str)]) -> TempDir {
        let root = TempDir::new().unwrap();
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        root
    }

    fn check(root: &Path, pins: &str) -> (Result<Outcome, Fault>, String) {
        let mut out = Vec::new();
        let command = MapCommand::CheckAnchors {
            root: root.to_owned(),
        };
        let outcome = run(&app(None, None), command, &mut pins.as_bytes(), &mut out);
        (outcome, String::from_utf8(out).unwrap())
    }

    const CRATE: &[(&str, &str)] = &[
        ("a/Cargo.toml", "[package]\nname = \"a\"\n"),
        ("a/src/lib.rs", "mod m;\npub fn run() { m::go(); }\n"),
        ("a/src/m.rs", "pub fn go() {}\n"),
    ];

    #[test]
    fn prints_missing_changed_and_malformed_pins() {
        let root = repo(CRATE);
        let pins = "sym:a::m::go#0000000000000000\nsym:a::gone#0000000000000000\nnot a pin\n";
        let (outcome, found) = check(&root, pins);
        assert_eq!(outcome.unwrap(), Outcome::Findings);
        let lines: Vec<&str> = found.lines().collect();
        assert_eq!(lines.len(), 3, "{found}");
        assert!(lines[0].starts_with("changed sym:a::m::go#"), "{found}");
        assert_eq!(lines[1..], ["missing sym:a::gone", "malformed line 3"]);

        let current = lines[0].trim_start_matches("changed ");
        let (outcome, found) = check(&root, current);
        assert_eq!((outcome.unwrap(), found), (Outcome::Done, String::new()));
    }

    #[test]
    fn reads_only_crate_sources() {
        let mut files = CRATE.to_vec();
        files.extend([
            ("a/src/notes.md", "fn hidden() {}"),
            (
                "a/target/CACHEDIR.TAG",
                "Signature: 8a477f597d28d172789f06886806bc55",
            ),
            ("a/target/debug/src/build.rs", "fn built() {}"),
            ("a/src/target/x.rs", "fn kept() {}"),
            ("a/src/bad.rs", "\u{0}"),
            ("a/tests/it.rs", "fn test() {}"),
            ("b/x.rs", "fn loose() {}"),
            ("a/.cache/x.rs", "fn cached() {}"),
            ("node_modules/b/Cargo.toml", "[package]\nname = \"b\"\n"),
        ]);
        let root = repo(&files);
        fs::write(root.join("a/src/bad.rs"), [0xff, 0xfe]).unwrap();
        let mut files = Vec::new();
        source_files(&root, "", &mut files).unwrap();
        let mut paths: Vec<String> = files.into_iter().map(|file| file.path).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "a/Cargo.toml",
                "a/src/lib.rs",
                "a/src/m.rs",
                "a/src/target/x.rs"
            ]
        );
    }

    #[test]
    fn skips_only_files_that_are_not_utf8() {
        let skipped = text_or_skip(Err(io::ErrorKind::InvalidData.into())).unwrap();
        assert_eq!(skipped, None);
        let failed = text_or_skip(Err(io::ErrorKind::PermissionDenied.into())).unwrap_err();
        assert_eq!(failed.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn fails_on_a_missing_root() {
        let dir = TempDir::new().unwrap();
        let root = dir.join("nowhere");
        let mut files = Vec::new();
        let error = source_files(&root, "", &mut files).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        let error = check(&root, "").0.unwrap_err();
        assert_eq!(error.code().as_str(), "map.read_failed");
    }

    #[test]
    fn fails_when_it_cant_read_or_print() {
        let root = repo(CRATE);
        let error =
            check_anchors(&app(None, None), &root, &mut Broken, &mut Vec::new()).unwrap_err();
        assert_eq!(error.code().as_str(), "map.input_failed");
        let pins = "sym:a::gone#0000000000000000";
        let error =
            check_anchors(&app(None, None), &root, &mut pins.as_bytes(), &mut Broken).unwrap_err();
        assert_eq!(error.code().as_str(), "map.output_failed");
    }
}
