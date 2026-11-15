use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Instant;

use weacon_appkit::Ending;
use weacon_codemap::{ContractKind, Index, SourceFile, index};
use weacon_observability::Fault;
use weacon_protocol::{FlowArgs, MapArgs, McpCall, SymbolArgs, WorkspaceId, code};

use crate::app::App;
use crate::cli::{ContractFilter, MapCommand, MapSubcommand};
use crate::codemap::{LARGEST, SEARCHED, index_failed, read_failed};
use crate::db::{BASE, Db, Layer, LocalRepo, Looking};
use crate::git::Git;
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
            answer(&McpCall::Symbol(SymbolArgs { path, budget }), &root)?
        }
        Some(MapSubcommand::Flows) => answer(&McpCall::Flow(FlowArgs { budget }), &root)?,
        Some(MapSubcommand::DocsFor { path }) => {
            answer(&McpCall::DocsFor(SymbolArgs { path, budget }), &root)?
        }
        Some(MapSubcommand::Search { words }) => search(&root, &words.join(" "), budget)?,
        Some(MapSubcommand::Hotspots) => hotspots(app, budget, &root)?,
        Some(MapSubcommand::Architecture) => answered(&root, budget, Index::architecture)?,
        Some(MapSubcommand::Contracts { kind }) => {
            let kind = kind.map(contract_kind);
            answered(&root, budget, |map, budget| contracts(map, kind, budget))?
        }
        None => answer(&McpCall::Map(MapArgs { budget }), &root)?,
    };
    print(out, &text)?;
    Ok(Ending::Done)
}

/// What the MCP's `hotspots` tool answers for `root`, weighed by the churn git counts there.
fn hotspots(app: &App, budget: Option<u32>, root: &Path) -> Result<String, Fault> {
    let git = Git::open(app.state_dir()?, app.path.as_deref())?;
    let churn = git.churn(root, tools::CHURN_COMMITS)?;
    let call = McpCall::Hotspots(MapArgs { budget });
    index_folder(root).map(|(map, _)| tools::answer_from(&call, &map, &churn))
}

const fn contract_kind(filter: ContractFilter) -> ContractKind {
    match filter {
        ContractFilter::Command => ContractKind::Command,
        ContractFilter::Code => ContractKind::ErrorCode,
        ContractFilter::Route => ContractKind::Route,
        ContractFilter::Serde => ContractKind::Serde,
        ContractFilter::Table => ContractKind::Table,
        ContractFilter::McpTool => ContractKind::McpTool,
    }
}

/// The code's contracts of `kind`, or of every kind, or a line saying it declares none.
fn contracts(map: &Index, kind: Option<ContractKind>, budget: usize) -> String {
    let listed = map.contracts(kind, budget);
    if listed.is_empty() {
        "No contracts.\n".to_owned()
    } else {
        listed
    }
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

/// What the MCP's tool answers to `call` from the code in `folder`, read now; it blocks.
fn answer(call: &McpCall, folder: &Path) -> Result<String, Fault> {
    index_folder(folder).map(|(map, _)| tools::answer_from(call, &map, &[]))
}

/// What the MCP's `search` tool answers for `words` from the code in `folder`, read now into a
/// map in memory; it blocks.
fn search(folder: &Path, words: &str, budget: Option<u32>) -> Result<String, Fault> {
    let (map, _) = index_folder(folder)?;
    let entries = map.entries();
    let hits = crate::app::runtime()?.block_on(async {
        let db = Db::memory().await?;
        // A workspace of its own, in a database no one else opens.
        let local = WorkspaceId::try_from("local".to_owned()).map_err(|_| read_failed())?;
        let map = db.workspace(&local).map(&LocalRepo::at(folder));
        let layer = Layer {
            name: BASE.to_owned(),
            commit: String::new(),
            generation: map.begin().await?,
            files: Vec::new(),
        };
        map.replace(&layer, &[], &entries).await?;
        map.search(BASE, words, Looking::Everything, SEARCHED).await
    })?;
    Ok(tools::found(&hits, tools::budget(budget)))
}

/// What `answer` gives the index of `folder` within `budget`, or that it has no map; it blocks.
fn answered(
    folder: &Path,
    budget: Option<u32>,
    answer: impl FnOnce(&Index, usize) -> String,
) -> Result<String, Fault> {
    let (map, _) = index_folder(folder)?;
    Ok(if map.crates.is_empty() {
        tools::NO_MAP.to_owned()
    } else {
        answer(&map, tools::budget(budget))
    })
}

/// [`answer`], or nothing when the folder has no code map.
///
/// # Errors
/// When the folder can't be read.
pub(crate) fn map_or_empty(call: &McpCall, folder: &Path) -> Result<String, Fault> {
    let (map, _) = index_folder(folder)?;
    Ok(if map.crates.is_empty() {
        String::new()
    } else {
        tools::answer_from(call, &map, &[])
    })
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
        let tool = |call: McpCall| answer(&call, &root).unwrap();
        let view = |root: &Path, view| printed(root, Some(view), None);

        let flows = tool(McpCall::Flow(FlowArgs::default()));
        assert_eq!(view(&root, MapSubcommand::Flows), flows);
        assert_eq!(
            view(&root, MapSubcommand::Architecture),
            index.architecture(6000)
        );
        assert_eq!(
            view(&root, MapSubcommand::Contracts { kind: None }),
            "No contracts.\n"
        );
        let serde = repo(&[
            ("Cargo.toml", "[package]\nname = \"a\"\n"),
            ("src/lib.rs", "#[derive(Serialize)]\npub struct Reply;\n"),
        ]);
        let (declared, _) = index_folder(&serde).unwrap();
        let contracts = declared.contracts(None, 6000);
        assert!(!contracts.is_empty());
        assert_eq!(
            view(&serde, MapSubcommand::Contracts { kind: None }),
            contracts
        );
        for subcommand in [
            MapSubcommand::Architecture,
            MapSubcommand::Contracts { kind: None },
        ] {
            assert_eq!(view(&empty, subcommand), "No code map for this folder.\n");
        }
        let none = "Flows pinned in the docs: 0.\n";
        assert_eq!(view(&empty, MapSubcommand::Flows), none);
    }

    #[test]
    fn hotspots_weigh_the_churn_git_counts_under_the_root() {
        let root = repo(CRATE);
        let (state, home) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let git = |args: &[&str]| crate::git::fixture(&home, &root, args);
        git(&["init", "--quiet"]);
        git(&["add", "--all"]);
        git(&["commit", "--quiet", "-m", "Start"]);
        fs::write(root.join("a/src/m.rs"), "pub fn go() { }\n").unwrap();
        git(&["commit", "--quiet", "--all", "-m", "Change"]);
        let app = App {
            path: std::env::var_os("PATH"),
            ..app(Some(&state), None)
        };
        let hotspots = |root: &Path, app: &App| {
            let mut out = Vec::new();
            let command = MapCommand {
                command: Some(MapSubcommand::Hotspots),
                root: root.to_owned(),
                budget: None,
            };
            run(app, command, &mut io::empty(), &mut out).map(|_| String::from_utf8(out).unwrap())
        };
        let title = "Hotspots: files that change often and that much depends on.\n";

        assert_eq!(
            hotspots(&root, &app).unwrap(),
            format!(
                "{title}- a/src/m.rs  2 commits, 1 edges in\n- a/src/lib.rs  1 commits, 0 edges in\n"
            )
        );
        assert_eq!(
            hotspots(&root.join("a"), &app).unwrap(),
            format!(
                "{title}- src/m.rs  2 commits, 1 edges in\n- src/lib.rs  1 commits, 0 edges in\n"
            )
        );
        let unrepo = hotspots(&state, &app).unwrap_err();
        let stateless = hotspots(&root, &crate::test_support::app(None, None)).unwrap_err();
        assert_eq!(unrepo.code().as_str(), "git.failed");
        assert_eq!(stateless.code().as_str(), "state.unavailable");
    }

    #[test]
    fn contracts_can_be_limited_to_one_kind() {
        let lib = "#[derive(Serialize)]\npub struct Reply;\n#[tool]\nfn search() {}\n\
                   fn fail() { code!(\"store.full\"); }\n\
                   fn router() { r.route(\"/version\", get(v)); }\n\
                   #[derive(Subcommand)]\nenum Command { Daemon }\n\
                   const SQL: &str = \"CREATE TABLE notes (id INTEGER)\";\n";
        let root = repo(&[
            ("Cargo.toml", "[package]\nname = \"a\"\n"),
            ("src/lib.rs", lib),
        ]);
        let kinds = [
            (
                ContractFilter::McpTool,
                "mcp tool search  a::search  src/lib.rs:3",
            ),
            (
                ContractFilter::Serde,
                "serde a::Reply  a::Reply  src/lib.rs:2",
            ),
            (
                ContractFilter::Code,
                "code store.full  a::fail  src/lib.rs:5",
            ),
            (
                ContractFilter::Route,
                "route GET /version  a::router  src/lib.rs:6",
            ),
            (
                ContractFilter::Command,
                "command daemon  a::Command  src/lib.rs:8",
            ),
            (ContractFilter::Table, "table notes  a::SQL  src/lib.rs:9"),
        ];
        for (kind, listed) in kinds {
            let view = MapSubcommand::Contracts { kind: Some(kind) };
            assert_eq!(printed(&root, Some(view), None), format!("- {listed}\n"));
        }
        let none = repo(&[
            ("Cargo.toml", "[package]\nname = \"a\"\n"),
            ("src/lib.rs", "fn a() {}\n"),
        ]);
        let tools = MapSubcommand::Contracts {
            kind: Some(ContractFilter::McpTool),
        };
        assert_eq!(printed(&none, Some(tools), None), "No contracts.\n");
    }

    #[test]
    fn a_flow_a_doc_pins_reaches_the_flow_tool() {
        let mut files = CRATE.to_vec();
        files.push((
            "docs/run.md",
            "Runs flow:a::run->a::m::go, then flow:a::run->a::gone.\n",
        ));
        let root = repo(&files);

        let flows = answer(&McpCall::Flow(FlowArgs::default()), &root).unwrap();

        assert_eq!(
            flows,
            "Flows pinned in the docs: 2.\n- docs/run.md:1  a::run -> a::m::go: holds\n\
             - docs/run.md:1  a::run -> a::gone: breaks: no item a::gone\n"
        );
        assert_eq!(printed(&root, Some(MapSubcommand::Flows), None), flows);
        let docs_for = MapSubcommand::DocsFor {
            path: "a::run".to_owned(),
        };
        assert_eq!(
            printed(&root, Some(docs_for), None),
            "Docs that pin a::run: 1.\n\
             - docs/run.md:1  Runs flow:a::run->a::m::go, then flow:a::run->a::gone.\n"
        );
        let docs_only = repo(&[("docs/run.md", "Runs flow:a::run->a::gone.\n")]);
        assert_eq!(
            printed(&docs_only, Some(MapSubcommand::Flows), None),
            "Flows pinned in the docs: 1.\n- docs/run.md:1  a::run -> a::gone: breaks: no item a::run\n"
        );
    }

    fn search(words: &[&str]) -> MapSubcommand {
        let words = words.iter().map(|word| (*word).to_owned()).collect();
        MapSubcommand::Search { words }
    }

    #[test]
    fn search_prints_the_items_and_docs_that_match_every_word_within_the_budget() {
        let many = (0..100).fold(String::new(), |text, n| {
            text + &format!("pub fn go_{n}() {{}}\n")
        });
        let mut files = CRATE.to_vec();
        files.extend([
            ("docs/run.md", "How a run goes.\n"),
            ("b/Cargo.toml", "[package]\nname = \"b\"\n"),
            ("b/src/lib.rs", &many),
        ]);
        let root = repo(&files);

        assert_eq!(
            printed(&root, Some(search(&["run"])), None),
            "Found 2:\n- fn a::run  a/src/lib.rs:2\n- doc docs/run.md  docs/run.md:1\n"
        );
        assert_eq!(
            printed(&root, Some(search(&["m", "go"])), None),
            "Found 2:\n- fn a::m::go  a/src/m.rs:1\n- doc docs/run.md  docs/run.md:1\n"
        );
        assert_eq!(printed(&root, Some(search(&["\" OR"])), None), "Found 0:\n");
        let cut = printed(&root, Some(search(&["go"])), Some(1000));
        assert!(cut.starts_with("Found 50:\n") && cut.len() <= 1000, "{cut}");
        assert!(printed(&root, Some(search(&["go"])), None).len() > 1000);
    }

    fn symbol(path: &str) -> MapSubcommand {
        let path = path.to_owned();
        MapSubcommand::Symbol { path }
    }

    #[test]
    fn map_and_symbol_print_exactly_what_the_tools_answer() {
        let root = repo(CRATE);
        let empty = TempDir::new().unwrap();
        let tool = |call: McpCall, root: &Path| answer(&call, root).unwrap();
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
        assert_eq!(cut, answer(&McpCall::Map(cut_by_tool), &root).unwrap());
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
