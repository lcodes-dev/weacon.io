use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;

use weacon_appkit::Ending;
use weacon_codemap::{Index, SourceFile, index};
use weacon_observability::Fault;
use weacon_protocol::{FlowArgs, MapArgs, McpCall, SymbolArgs, code};

use crate::app::App;
use crate::cli::{MapCommand, MapSubcommand};
use crate::mcp::tools;

/// Prints what the MCP's tools answer, or checks the repository's pins: `Findings` when one
/// no longer matches.
pub(crate) fn run(
    app: &App,
    command: MapCommand,
    input: &mut impl Read,
    out: &mut impl Write,
) -> Result<Ending, Fault> {
    let MapCommand {
        command,
        root,
        budget,
    } = command;
    let text = match command {
        Some(MapSubcommand::CheckAnchors) => return check_anchors(app, &root, input, out),
        Some(MapSubcommand::Symbol { path }) => {
            tools::answer(&McpCall::Symbol(SymbolArgs { path, budget }), &root)?
        }
        Some(MapSubcommand::Flows) => tools::answer(&McpCall::Flow(FlowArgs { budget }), &root)?,
        Some(MapSubcommand::Architecture) => tools::answered(&root, budget, Index::architecture)?,
        Some(MapSubcommand::Contracts) => {
            tools::answered(&root, budget, |map, budget| map.contracts(None, budget))?
        }
        None => tools::answer(&McpCall::Map(MapArgs { budget }), &root)?,
    };
    print(out, &text)?;
    Ok(Ending::Done)
}

/// Prints each pin read from `input` that no longer matches the repository at `root`.
fn check_anchors(
    app: &App,
    root: &Path,
    input: &mut impl Read,
    out: &mut impl Write,
) -> Result<Ending, Fault> {
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
    print(out, &report)?;
    Ok(if findings.is_empty() {
        Ending::Done
    } else {
        Ending::Findings
    })
}

fn print(out: &mut impl Write, text: &str) -> Result<(), Fault> {
    out.write_all(text.as_bytes()).map_err(|error| {
        Fault::new(code!("map.output_failed"), "The answer couldn't be printed").because(error)
    })
}

/// The code map of the repository at `root`, and how many files it read; it blocks.
pub(crate) fn index_folder(root: &Path) -> Result<(Index, usize), Fault> {
    let mut files = Vec::new();
    source_files(root, "", &mut files).map_err(|error| {
        Fault::new(code!("map.read_failed"), "The repository couldn't be read").because(error)
    })?;
    let index = index(&files).map_err(|error| {
        Fault::new(code!("map.index_failed"), "The code map couldn't be built").because(error)
    })?;
    Ok((index, files.len()))
}

/// Every `Cargo.toml`, and every `.rs` file inside a `src` folder, under `root/folder`, its path
/// joined with `/`. Hidden entries, links, `node_modules`, Cargo's build folders and files
/// that aren't UTF-8 are left out.
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
            && is_source(&path, name)
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

    fn check(root: &Path, pins: &str) -> (Result<Ending, Fault>, String) {
        let mut out = Vec::new();
        let command = MapCommand {
            command: Some(MapSubcommand::CheckAnchors),
            root: root.to_owned(),
            budget: None,
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

    fn printed(root: &Path, command: Option<MapSubcommand>, budget: Option<u32>) -> String {
        let mut out = Vec::new();
        let command = MapCommand {
            command,
            root: root.to_owned(),
            budget,
        };
        let ending = run(&app(None, None), command, &mut io::empty(), &mut out);
        assert_eq!(ending.unwrap(), Ending::Done);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn each_view_prints_its_answer_and_the_tools_views_match_them() {
        let root = repo(CRATE);
        let empty = TempDir::new().unwrap();
        let (index, _) = index_folder(&root).unwrap();
        let tool = |call: McpCall| tools::answer(&call, &root).unwrap();
        let view = |root: &Path, view| printed(root, Some(view), None);

        let flows = tool(McpCall::Flow(FlowArgs::default()));
        assert_eq!(view(&root, MapSubcommand::Flows), flows);
        assert_eq!(
            view(&root, MapSubcommand::Architecture),
            index.architecture(6000)
        );
        assert_eq!(
            view(&root, MapSubcommand::Contracts),
            index.contracts(None, 6000)
        );
        let views = [
            MapSubcommand::Flows,
            MapSubcommand::Architecture,
            MapSubcommand::Contracts,
        ];
        for subcommand in views {
            assert_eq!(view(&empty, subcommand), "No code map for this folder.\n");
        }
    }

    fn symbol(path: &str) -> MapSubcommand {
        let path = path.to_owned();
        MapSubcommand::Symbol { path }
    }

    #[test]
    fn map_and_symbol_print_exactly_what_the_tools_answer() {
        let root = repo(CRATE);
        let empty = TempDir::new().unwrap();
        let tool = |call: McpCall, root: &Path| tools::answer(&call, root).unwrap();
        let go = || SymbolArgs {
            path: "a::m::go".into(),
            budget: None,
        };

        let outline = printed(&root, None, None);
        let item = printed(&root, Some(symbol("a::m::go")), None);

        assert_eq!(outline, tool(McpCall::Map(MapArgs::default()), &root));
        assert_eq!(item, tool(McpCall::Symbol(go()), &root));
        assert_eq!(
            item,
            "fn a::m::go  a/src/m.rs:1\ncalled by fn a::run  a/src/lib.rs:2\n"
        );
        let none = tool(McpCall::Map(MapArgs::default()), &empty);
        assert_eq!(printed(&empty, None, None), none);
    }

    #[test]
    fn a_budget_reaches_the_map_tool() {
        let root = repo(CRATE);
        for n in 0..60 {
            let folder = root.join(format!("crate_number_{n}"));
            fs::create_dir_all(folder.join("src")).unwrap();
            let manifest = format!("[package]\nname = \"crate_number_{n}\"\n");
            fs::write(folder.join("Cargo.toml"), manifest).unwrap();
            fs::write(folder.join("src/lib.rs"), "pub fn run() {}\n").unwrap();
        }
        let cut_by_tool = MapArgs { budget: Some(1000) };

        let whole = printed(&root, None, None);
        let cut = printed(&root, None, Some(1000));

        assert_ne!(cut, whole);
        assert_eq!(
            cut,
            tools::answer(&McpCall::Map(cut_by_tool), &root).unwrap()
        );
    }

    #[test]
    fn a_root_that_cant_be_read_or_an_answer_that_cant_be_printed_is_an_error() {
        let command = |root: std::path::PathBuf| MapCommand {
            command: None,
            root,
            budget: None,
        };
        let run = |command, out: &mut dyn Write| {
            run(&app(None, None), command, &mut io::empty(), &mut &mut *out).unwrap_err()
        };
        let dir = TempDir::new().unwrap();

        let unread = run(command(dir.join("nowhere")), &mut Vec::new());
        let unprinted = run(command(repo(CRATE).to_path_buf()), &mut Broken);

        assert_eq!(unread.code().as_str(), "map.read_failed");
        assert_eq!(unprinted.code().as_str(), "map.output_failed");
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
