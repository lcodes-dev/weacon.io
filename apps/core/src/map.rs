use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;

use weacon_appkit::Ending;
use weacon_codemap::{Index, SourceFile, index};
use weacon_observability::Fault;
use weacon_protocol::code;

use crate::app::App;
use crate::cli::MapCommand;
use crate::codemap::{LARGEST, index_failed, read_failed};

/// Checks the repository; `Findings` when a pin no longer matches.
pub(crate) fn run(
    app: &App,
    command: MapCommand,
    input: &mut impl Read,
    out: &mut impl Write,
) -> Result<Ending, Fault> {
    let MapCommand::CheckAnchors { root } = command;
    let clean = check_anchors(app, &root, input, out)?;
    Ok(if clean {
        Ending::Done
    } else {
        Ending::Findings
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
    let (index, files) = index_folder(root)?;
    let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    app.log.info(
        "map indexed",
        &[("files", files.into()), ("ms", millis.into())],
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

/// The code map of the repository at `root`, and how many files it read; it blocks.
pub(crate) fn index_folder(root: &Path) -> Result<(Index, usize), Fault> {
    let mut files = Vec::new();
    source_files(root, "", &mut files).map_err(|error| read_failed().because(error))?;
    let index = index(&files).map_err(|error| index_failed().because(error))?;
    Ok((index, files.len()))
}

/// Every file the code map takes under `root/folder`, its path joined with `/`. Hidden entries,
/// links, `node_modules`, Cargo's build folders, files over [`LARGEST`] bytes and files that
/// aren't UTF-8 are left out.
fn source_files(root: &Path, folder: &str, files: &mut Vec<SourceFile>) -> io::Result<()> {
    for entry in fs::read_dir(root.join(folder))? {
        let Some(entry) = present(entry)? else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| !name.starts_with('.')) else {
            continue;
        };
        let path = if folder.is_empty() {
            name.to_owned()
        } else {
            format!("{folder}/{name}")
        };
        let Some(kind) = present(entry.file_type())? else {
            continue;
        };
        if kind.is_dir() && !skipped(&root.join(&path), name) {
            present(source_files(root, &path, files))?;
        } else if kind.is_file()
            && weacon_codemap::is_mapped(&path)
            && present(entry.metadata())?.is_some_and(|about| about.len() <= LARGEST)
            && let Some(text) = text_or_skip(fs::read_to_string(root.join(&path)))?
        {
            files.push(SourceFile { path, text });
        }
    }
    Ok(())
}

/// A file's text, or `None` for a file that isn't UTF-8 or was removed while the folder was read.
fn text_or_skip(read: io::Result<String>) -> io::Result<Option<String>> {
    match read {
        Err(error) if error.kind() == io::ErrorKind::InvalidData => Ok(None),
        read => present(read),
    }
}

/// What was read, or `None` for an entry removed while its folder was read.
fn present<T>(read: io::Result<T>) -> io::Result<Option<T>> {
    match read {
        Ok(read) => Ok(Some(read)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// `node_modules`, and folders Cargo marks as its build output.
fn skipped(folder: &Path, name: &str) -> bool {
    name == "node_modules" || folder.join("CACHEDIR.TAG").is_file()
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

    fn check(root: &Path, pins: &str) -> (Result<Ending, Fault>, String) {
        let mut out = Vec::new();
        let command = MapCommand::CheckAnchors {
            root: root.to_owned(),
        };
        let ending = run(&app(None, None), command, &mut pins.as_bytes(), &mut out);
        (ending, String::from_utf8(out).unwrap())
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
        let (ending, found) = check(&root, pins);
        assert_eq!(ending.unwrap(), Ending::Findings);
        let lines: Vec<&str> = found.lines().collect();
        assert_eq!(lines.len(), 3, "{found}");
        assert!(lines[0].starts_with("changed sym:a::m::go#"), "{found}");
        assert_eq!(lines[1..], ["missing sym:a::gone", "malformed line 3"]);

        let current = lines[0].trim_start_matches("changed ");
        let (ending, found) = check(&root, current);
        assert_eq!((ending.unwrap(), found), (Ending::Done, String::new()));
    }

    #[test]
    fn reads_only_crate_sources() {
        let mut files = CRATE.to_vec();
        files.extend([
            ("a/src/notes.txt", "fn hidden() {}"),
            ("a/notes.md", "# Notes\n"),
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
        let largest = usize::try_from(LARGEST).unwrap();
        fs::write(root.join("a/src/full.rs"), " ".repeat(largest)).unwrap();
        fs::write(root.join("a/src/huge.rs"), " ".repeat(largest + 1)).unwrap();
        let mut files = Vec::new();
        source_files(&root, "", &mut files).unwrap();
        let mut paths: Vec<String> = files.into_iter().map(|file| file.path).collect();
        paths.sort();
        assert_eq!(
            paths,
            [
                "a/Cargo.toml",
                "a/notes.md",
                "a/src/full.rs",
                "a/src/lib.rs",
                "a/src/m.rs",
                "a/src/target/x.rs"
            ]
        );
    }

    #[test]
    fn skips_only_files_that_are_not_utf8_or_were_removed_meanwhile() {
        for kind in [io::ErrorKind::InvalidData, io::ErrorKind::NotFound] {
            assert_eq!(text_or_skip(Err(kind.into())).unwrap(), None);
        }
        assert_eq!(
            present(Err::<(), _>(io::ErrorKind::NotFound.into())).unwrap(),
            None
        );
        let denied = present(Err::<(), _>(io::ErrorKind::PermissionDenied.into())).unwrap_err();
        assert_eq!(denied.kind(), io::ErrorKind::PermissionDenied);
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
