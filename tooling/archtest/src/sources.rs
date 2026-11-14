//! Checks of each package's source files: tests, I/O, where code lives, size and leftovers.

use std::collections::BTreeSet;
use std::path::Path;

use syn::buffer::{Cursor, TokenBuffer};
use syn::visit::{self, Visit};
use syn::{Attribute, Ident, ItemFn, ItemMod, ItemUse, Macro, Meta, UseTree};

use crate::SourceFile;
use crate::rules::{DB_DRIVERS, IO_MODULES, MARKERS, MAX_CODE_LINES, OS_CFGS, Rule};

pub(crate) fn check_sources(rule: &Rule, sources: &[SourceFile]) -> Vec<String> {
    let name = rule.package;
    let mut out = Vec::new();
    if !sources
        .iter()
        .any(|file| file.text.contains("#[test]") || file.text.contains("#[tokio::test]"))
    {
        out.push(format!("{name}: has no tests"));
    }
    for file in sources {
        let path = file.path.display();
        out.extend(
            check_file(rule, file)
                .into_iter()
                .map(|problem| format!("{name}: {path} {problem}")),
        );
    }
    out
}

fn check_file(rule: &Rule, file: &SourceFile) -> Vec<String> {
    let mut out = check_text(&file.text);
    match Uses::of(&file.text) {
        Ok(uses) => out.extend(check_uses(rule, &file.path, &uses)),
        Err(error) => out.push(format!("can't be parsed: {error}")),
    }
    out
}

fn check_text(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let code = text
        .split("#[cfg(test)]\nmod tests")
        .next()
        .unwrap_or_default();
    let code_lines = code.lines().count();
    if code_lines > MAX_CODE_LINES {
        out.push(format!(
            "has {code_lines} lines before its tests; split it to stay within {MAX_CODE_LINES}"
        ));
    }
    let markers: BTreeSet<&str> = words(text).filter(|word| MARKERS.contains(word)).collect();
    out.extend(
        markers
            .iter()
            .map(|marker| format!("has a {marker}; finish the work or remove the note")),
    );
    out
}

fn check_uses(rule: &Rule, path: &Path, uses: &Uses) -> Vec<String> {
    let mut out = Vec::new();
    if !rule.io {
        out.extend(
            uses.std_modules
                .iter()
                .filter(|module| IO_MODULES.contains(&module.as_str()))
                .map(|module| format!("uses std::{module}, but its package does no I/O")),
        );
    }
    if path.starts_with("src") {
        if !in_module(path, "db") {
            out.extend(
                uses.roots
                    .iter()
                    .filter(|root| DB_DRIVERS.contains(&root.as_str()))
                    .map(|driver| format!("uses {driver} outside the db module")),
            );
        }
        if uses.os_cfg && !in_module(path, "platform") {
            out.push("selects an operating system outside the platform module".to_owned());
        }
    }
    out
}

// `src/<name>.rs`, or anything under `src/<name>/`.
fn in_module(path: &Path, name: &str) -> bool {
    path.iter()
        .nth(1)
        .and_then(|part| Path::new(part).file_stem())
        .is_some_and(|stem| stem == name)
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .filter(|word| !word.is_empty())
}

// Whether `cfg` arguments name an operating system; quoted values, such as feature names, don't.
fn names_os(arguments: &str) -> bool {
    arguments
        .split('"')
        .step_by(2)
        .flat_map(words)
        .any(|word| OS_CFGS.contains(&word))
}

/// What a file's code refers to outside `#[cfg(test)]` modules, functions and `use`s.
#[derive(Default)]
struct Uses {
    /// The first segment of every `use` and of every path with two or more segments.
    roots: BTreeSet<String>,
    std_modules: BTreeSet<String>,
    os_cfg: bool,
}

impl Uses {
    fn of(text: &str) -> Result<Self, String> {
        let file = syn::parse_file(text).map_err(|error| error.to_string())?;
        let mut uses = Self::default();
        uses.visit_file(&file);
        Ok(uses)
    }

    fn note<'a>(&mut self, mut segments: impl Iterator<Item = &'a Ident>) {
        let Some(root) = segments.next().map(ToString::to_string) else {
            return;
        };
        if root == "std"
            && let Some(module) = segments.next()
        {
            self.std_modules.insert(module.to_string());
        }
        self.roots.insert(root);
    }

    /// Notes the paths in a macro's tokens, which `syn` leaves unparsed, each after `prefix`.
    fn note_tokens(&mut self, mut cursor: Cursor<'_>, prefix: &[Ident]) {
        let mut path = Vec::new();
        let mut colons = 0;
        while let Some((_, next)) = cursor.token_tree() {
            colons = self.note_token(cursor, prefix, &mut path, colons);
            cursor = next;
        }
        self.note_path(prefix, &mut path);
    }

    // Takes the token at `cursor` into `path`, and returns how many colons now end it.
    fn note_token(
        &mut self,
        cursor: Cursor<'_>,
        prefix: &[Ident],
        path: &mut Vec<Ident>,
        colons: u8,
    ) -> u8 {
        if let Some((punct, _)) = cursor.punct()
            && punct.as_char() == ':'
        {
            return colons.saturating_add(1);
        }
        if let Some((ident, _)) = cursor.ident() {
            if colons != 2 {
                self.note_path(prefix, path);
            }
            path.push(ident);
            return 0;
        }
        // A group straight after `::` continues the path, as in `std::{fs, env}`.
        let outer = if colons == 2 {
            [prefix, path].concat()
        } else {
            Vec::new()
        };
        self.note_path(prefix, path);
        if let Some((inside, ..)) = cursor.any_group() {
            self.note_tokens(inside, &outer);
        }
        0
    }

    fn note_path(&mut self, prefix: &[Ident], path: &mut Vec<Ident>) {
        if prefix.len() + path.len() > 1 {
            self.note(prefix.iter().chain(path.iter()));
        }
        path.clear();
    }

    fn note_tree(&mut self, tree: &UseTree, prefix: &mut Vec<Ident>) {
        match tree {
            UseTree::Path(path) => {
                prefix.push(path.ident.clone());
                self.note_tree(&path.tree, prefix);
                prefix.pop();
            }
            UseTree::Name(name) => self.note(prefix.iter().chain([&name.ident])),
            UseTree::Rename(rename) => self.note(prefix.iter().chain([&rename.ident])),
            UseTree::Glob(_) => self.note(prefix.iter()),
            UseTree::Group(group) => {
                for item in &group.items {
                    self.note_tree(item, prefix);
                }
            }
        }
    }
}

fn is_test_only(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<Ident>()
                .is_ok_and(|argument| argument == "test")
    })
}

impl<'ast> Visit<'ast> for Uses {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !is_test_only(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        if !is_test_only(&item.attrs) {
            visit::visit_item_fn(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        if !is_test_only(&item.attrs) {
            self.note_tree(&item.tree, &mut Vec::new());
            visit::visit_item_use(self, item);
        }
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        if path.segments.len() > 1 {
            self.note(path.segments.iter().map(|segment| &segment.ident));
        }
        visit::visit_path(self, path);
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        if (attr.path().is_ident("cfg") || attr.path().is_ident("cfg_attr"))
            && let Meta::List(list) = &attr.meta
        {
            self.os_cfg |= names_os(&list.tokens.to_string());
        }
        visit::visit_attribute(self, attr);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        if mac.path.is_ident("cfg") {
            self.os_cfg |= names_os(&mac.tokens.to_string());
        }
        self.note_tokens(TokenBuffer::new2(mac.tokens.clone()).begin(), &[]);
        visit::visit_macro(self, mac);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const PURE: Rule = Rule {
        package: "weacon-pure",
        may_use: &[],
        io: false,
    };
    const IO: Rule = Rule {
        package: "weacon-core",
        may_use: &[],
        io: true,
    };

    fn file(path: &str, text: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            text: text.to_owned(),
        }
    }

    fn check(rule: &Rule, path: &str, text: &str) -> Vec<String> {
        check_file(rule, &file(path, text))
    }

    #[test]
    fn packages_without_tests_are_flagged() {
        assert_eq!(
            check_sources(&PURE, &[file("src/lib.rs", "fn main() {}")]),
            ["weacon-pure: has no tests"]
        );
        assert!(check_sources(&PURE, &[file("src/lib.rs", "#[test]\nfn works() {}")]).is_empty());
    }

    #[test]
    fn finds_io_modules_outside_comments_and_tests() {
        let io = |module: &str| format!("uses std::{module}, but its package does no I/O");
        let cases = [
            (
                "use std::{collections::BTreeMap, env};\nuse std::{io::Read, net::TcpStream};",
                vec![io("env"), io("net")],
            ),
            (
                "fn read() -> Vec<u8> { std::fs::read(\"x\").unwrap_or_default() }",
                vec![io("fs")],
            ),
            (
                "mod spawn { use std::process::Child; }",
                vec![io("process")],
            ),
            ("// std::process in a comment", vec![]),
            ("#[cfg(test)]\nuse std::net::TcpStream;", vec![]),
            (
                "#[cfg(test)]\nmod tests { use std::process::Command; }",
                vec![],
            ),
            (
                "#[cfg(test)]\nfn helper() { std::fs::read(\"x\"); }",
                vec![],
            ),
        ];

        for (text, found) in cases {
            assert_eq!(check(&PURE, "src/lib.rs", text), found, "{text}");
            assert!(check(&IO, "src/lib.rs", text).is_empty(), "{text}");
        }
    }

    #[test]
    fn finds_io_modules_inside_macro_calls() {
        let io = |module: &str| format!("uses std::{module}, but its package does no I/O");
        let cases = [
            (
                "fn home() -> String { format!(\"home\", std::env::var(\"HOME\")) }",
                vec![io("env")],
            ),
            (
                "fn all() -> Vec<u8> { vec![std::fs::read(\"x\").map(|_| 0).unwrap_or(1)] }",
                vec![io("fs")],
            ),
            (
                "fn check() { assert!(matches!(Some(1), Some(_)), \"pid\", { std::process::id() }); }",
                vec![io("process")],
            ),
            (
                "fn f() { assert!(std :: net :: Ipv4Addr::LOCALHOST.is_loopback()); }",
                vec![io("net")],
            ),
            (
                "fn f() -> String { format!(\"std::fs\", std::fmt::Error) }",
                vec![],
            ),
            ("fn f() { let x: fs::File; m!(std: fs::File); }", vec![]),
            ("fn f() { m!(std::fs::File); }", vec![io("fs")]),
            (
                "fn f() { m!(std::{fs::File, env}, net); }",
                vec![io("env"), io("fs")],
            ),
            (
                "macro_rules! t { () => { use std::{process}; }; }",
                vec![io("process")],
            ),
        ];

        for (text, found) in cases {
            assert_eq!(check(&PURE, "src/lib.rs", text), found, "{text}");
        }
    }

    #[test]
    fn database_drivers_stay_in_the_db_module() {
        let text = "use sqlx::PgPool;\nfn rows() { tokio_postgres::connect(); }";

        assert_eq!(
            check(&IO, "src/chats.rs", text),
            [
                "uses sqlx outside the db module",
                "uses tokio_postgres outside the db module",
            ]
        );
        assert!(check(&IO, "src/db.rs", text).is_empty());
        assert!(check(&IO, "src/db/chats.rs", text).is_empty());
        assert!(check(&IO, "tests/store.rs", text).is_empty());
        for text in [
            "fn rows(postgres: u8) -> u8 { postgres }",
            "fn rows(postgres: u8) -> String { format!(\"rows\", postgres) }",
        ] {
            assert!(check(&IO, "src/chats.rs", text).is_empty(), "{text}");
        }
        assert_eq!(
            check(&IO, "src/chats.rs", "fn rows() { m!(a::b sqlx::query) }"),
            ["uses sqlx outside the db module"]
        );
    }

    #[test]
    fn operating_systems_are_selected_only_in_the_platform_module() {
        let attribute = "#[cfg(target_os = \"macos\")]\nfn trash() {}";
        let call = "fn sep() -> char { if cfg!(windows) { '\\\\' } else { '/' } }";
        let feature = "#[cfg(feature = \"windows\")]\nfn trash() {}";

        for text in [attribute, call] {
            assert_eq!(
                check(&IO, "src/git.rs", text),
                ["selects an operating system outside the platform module"]
            );
            assert!(check(&IO, "src/platform.rs", text).is_empty());
            assert!(check(&IO, "src/platform/macos.rs", text).is_empty());
        }
        assert!(check(&IO, "src/git.rs", feature).is_empty());
        assert!(check(&IO, "src/git.rs", "#[cfg(test)]\n#[cfg(unix)]\nfn f() {}").is_empty());
    }

    #[test]
    fn leftover_markers_and_long_files_are_flagged() {
        let marker = format!("// {}: later\nfn f() {{}}", concat!("TO", "DO"));
        let long = "fn f() {}\n".repeat(MAX_CODE_LINES + 1);
        let long_tests = format!("fn f() {{}}\n#[cfg(test)]\nmod tests {{\n{long}}}");

        assert_eq!(
            check(&IO, "src/lib.rs", &marker),
            [format!(
                "has a {}; finish the work or remove the note",
                concat!("TO", "DO")
            )]
        );
        assert!(check(&IO, "src/lib.rs", "// TODOS and a todo are fine").is_empty());
        assert_eq!(
            check(&IO, "src/lib.rs", &long),
            [format!(
                "has {} lines before its tests; split it to stay within {MAX_CODE_LINES}",
                MAX_CODE_LINES + 1
            )]
        );
        assert!(check(&IO, "src/lib.rs", &long_tests).is_empty());
        assert!(check(&IO, "src/lib.rs", &"fn f() {}\n".repeat(MAX_CODE_LINES)).is_empty());
        assert_eq!(
            check(
                &IO,
                "src/lib.rs",
                &format!("#[cfg(test)]\nuse std::sync::Arc;\n{long}")
            )
            .len(),
            1
        );
    }

    #[test]
    fn unparsable_files_are_reported() {
        let found = check(&IO, "src/lib.rs", "fn {");

        assert_eq!(found.len(), 1);
        assert!(found[0].starts_with("can't be parsed: "), "{found:?}");
    }
}
