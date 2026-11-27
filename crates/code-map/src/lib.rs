use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use serde::Serialize;
use tree_sitter::{Node, Parser, Query, QueryCursor, StreamingIterator};

mod link;

pub use link::{File, link};

const COMPONENTS: [&str; 2] = ["Vue", "Svelte"];

const MAX_LINE: usize = 5_000;

const FOREIGN: [&str; 2] = ["vendor", "node_modules"];

const MEMBER: [&str; 6] = [".", "->", "::", "?.", "?->", "&."];

pub struct Language {
    pub name: &'static str,
    family: &'static str,
    extensions: &'static [&'static str],
    separator: &'static str,
    index: &'static str,
    grammar: tree_sitter::Language,
    query: Query,
}

#[rustfmt::skip]
static LANGUAGES: LazyLock<Vec<Language>> = LazyLock::new(|| {
    use {
        tree_sitter_bash as bash, tree_sitter_c as c, tree_sitter_c_sharp as c_sharp,
        tree_sitter_cpp as cpp, tree_sitter_dart as dart, tree_sitter_elixir as elixir,
        tree_sitter_go as go, tree_sitter_java as java, tree_sitter_javascript as javascript,
        tree_sitter_kotlin_ng as kotlin, tree_sitter_lua as lua, tree_sitter_php as php,
        tree_sitter_python as python, tree_sitter_ruby as ruby, tree_sitter_rust as rust,
        tree_sitter_scala as scala, tree_sitter_swift as swift,
        tree_sitter_typescript as typescript, tree_sitter_zig as zig,
    };
    macro_rules! ours {
        ($name:literal) => {
            include_str!(concat!("../queries/", $name, ".scm"))
        };
    }
    type Grammar = tree_sitter::Language;
    let language = |name, family, extensions, separator, index, grammar: Grammar, queries: &[&str]| {
        let query = Query::new(&grammar, &queries.join("\n"))
            .unwrap_or_else(|error| panic!("{name}'s query doesn't fit: {error}"));
        Language { name, family, extensions, separator, index, grammar, query }
    };
    let (ts, tsx) = (typescript::LANGUAGE_TYPESCRIPT, typescript::LANGUAGE_TSX);
    let js = [javascript::TAGS_QUERY, ours!("jsx"), ours!("javascript")];
    let typescript = [js[0], typescript::TAGS_QUERY, ours!("typescript"), js[2]];
    let tsx_queries = [&typescript[..], &js[1..2]].concat();
    let cpp_queries = [c::TAGS_QUERY, cpp::TAGS_QUERY, ours!("c")];
    let c_sharp_tags = c_sharp::TAGS_QUERY.replace(") @module", ") @definition.module");
    vec![
        language("JavaScript", "js", &["js", "jsx", "mjs", "cjs"], "/", "index", javascript::LANGUAGE.into(), &js),
        language("TypeScript", "js", &["ts", "mts", "cts"], "/", "index", ts.into(), &typescript),
        language("TypeScript", "js", &["tsx"], "/", "index", tsx.into(), &tsx_queries),
        language("Vue", "js", &["vue"], "/", "index", ts.into(), &typescript),
        language("Svelte", "js", &["svelte"], "/", "index", ts.into(), &typescript),
        language("Rust", "rust", &["rs"], "::", "mod", rust::LANGUAGE.into(), &[rust::TAGS_QUERY, ours!("rust")]),
        language("Python", "python", &["py"], ".", "__init__", python::LANGUAGE.into(), &[python::TAGS_QUERY, ours!("python")]),
        language("PHP", "php", &["php"], "\\", "", php::LANGUAGE_PHP.into(), &[php::TAGS_QUERY, ours!("php")]),
        language("Go", "go", &["go"], "/", "", go::LANGUAGE.into(), &[go::TAGS_QUERY, ours!("go")]),
        language("Java", "jvm", &["java"], ".", "", java::LANGUAGE.into(), &[java::TAGS_QUERY, ours!("java")]),
        language("C", "c", &["c", "h"], "/", "", c::LANGUAGE.into(), &[c::TAGS_QUERY, ours!("c")]),
        language("C++", "c", &["cpp", "cc", "cxx", "hpp", "hh", "hxx"], "/", "", cpp::LANGUAGE.into(), &cpp_queries),
        language("C#", "c#", &["cs"], ".", "", c_sharp::LANGUAGE.into(), &[&c_sharp_tags, ours!("c_sharp")]),
        language("Ruby", "ruby", &["rb"], "/", "", ruby::LANGUAGE.into(), &[ruby::TAGS_QUERY, ours!("ruby")]),
        language("Swift", "swift", &["swift"], ".", "", swift::LANGUAGE.into(), &[swift::TAGS_QUERY, ours!("swift")]),
        language("Dart", "dart", &["dart"], "/", "", dart::LANGUAGE.into(), &[dart::TAGS_QUERY, ours!("dart")]),
        language("Elixir", "elixir", &["ex", "exs"], ".", "", elixir::LANGUAGE.into(), &[elixir::TAGS_QUERY, ours!("elixir")]),
        language("Lua", "lua", &["lua"], ".", "init", lua::LANGUAGE.into(), &[lua::TAGS_QUERY, ours!("lua")]),
        language("Kotlin", "jvm", &["kt", "kts"], ".", "", kotlin::LANGUAGE.into(), &[ours!("kotlin")]),
        language("Scala", "jvm", &["scala", "sc"], ".", "", scala::LANGUAGE.into(), &[ours!("scala")]),
        language("Bash", "bash", &["sh", "bash"], "/", "", bash::LANGUAGE.into(), &[ours!("bash")]),
        language("Zig", "zig", &["zig"], "/", "", zig::LANGUAGE.into(), &[ours!("zig")]),
    ]
});

#[derive(Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: String,
    pub line: i64,
    pub end_line: i64,
}

#[derive(Default)]
pub struct Reading {
    pub lines: i64,
    pub symbols: Vec<Symbol>,
    pub scope: Option<String>,
    pub imports: BTreeSet<String>,
    pub mentions: BTreeSet<(String, bool)>,
    pub text: Option<String>,
}

pub fn language(path: &str) -> Option<&'static Language> {
    let extension = path.rsplit('.').next().unwrap_or_default();
    LANGUAGES.iter().find(|language| language.extensions.contains(&extension))
}

pub fn key(path: &str, blob: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    if foreign(path) {
        return format!("{blob} /");
    }
    match language(path) {
        Some(language) if COMPONENTS.contains(&language.name) => format!("{blob} {name}"),
        Some(_) => format!("{blob} {}", name.rsplit('.').next().unwrap_or_default()),
        None => blob.to_owned(),
    }
}

fn foreign(path: &str) -> bool {
    let folders = path.rsplit_once('/').map_or("", |(folders, _)| folders);
    folders.split('/').any(|folder| FOREIGN.contains(&folder))
}

fn blank(text: &str) -> impl Iterator<Item = char> {
    text.bytes().map(|byte| if byte == b'\n' { '\n' } else { ' ' })
}

fn uncommented(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!--") {
        let end = rest[at..].find("-->").map_or(rest.len(), |end| at + end + 3);
        kept.push_str(&rest[..at]);
        kept.extend(blank(&rest[at..end]));
        rest = &rest[end..];
    }
    kept + rest
}

fn scripts(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let opened = rest.find("<script");
        let script = opened.and_then(|at| Some(at + rest[at..].find('>')? + 1));
        let (markup, after) = rest.split_at(script.unwrap_or(rest.len()));
        kept.extend(blank(markup));
        let end = after.find("</script").unwrap_or(after.len());
        kept.push_str(&after[..end]);
        rest = &after[end..];
    }
    kept
}

pub fn read(path: &str, bytes: &[u8]) -> Reading {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Reading::default();
    };
    let mut reading = Reading { lines: text.lines().count() as i64, ..Reading::default() };
    if foreign(path) || text.lines().any(|line| line.len() > MAX_LINE) {
        return reading;
    }
    reading.text = Some(text.to_owned()).filter(|text| !text.is_empty());
    let Some(language) = language(path) else {
        return reading;
    };
    let component = COMPONENTS.contains(&language.name);
    let markup = component.then(|| uncommented(text));
    let scripts = markup.as_deref().map(scripts);
    let source = scripts.as_deref().unwrap_or(text);
    let mut parser = Parser::new();
    let parsed = parser.set_language(&language.grammar).ok();
    let Some(tree) = parsed.and_then(|()| parser.parse(source, None)) else {
        return reading;
    };

    let said = |node: Node| source[node.byte_range()].to_owned();
    let bare = |node: Node| {
        let quotes: &[char] = &['"', '\'', '`', '<', '>'];
        said(node).trim_matches(quotes).to_owned()
    };
    let captures = language.query.capture_names();
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&language.query, tree.root_node(), source.as_bytes());
    while let Some(found) = matches.next() {
        let (mut name, mut definition, mut reference) = (None, None, false);
        let (mut import, mut imported) = (None, Vec::new());
        for capture in found.captures() {
            let node = capture.node;
            match captures[capture.index as usize] {
                "name" => name = Some(node),
                "mention" => mention(&mut reading, node, said(node)),
                "scope" => reading.scope = Some(said(node)),
                "import" => import = Some(bare(node)),
                "import.line" => (import, imported) = line(&said(node)),
                "import.name" => imported.push(bare(node)),
                capture => match capture.split_once('.') {
                    Some(("definition", kind)) => definition = Some((kind, node)),
                    Some(("reference", _)) => reference = true,
                    _ => {}
                },
            }
        }
        match (name, definition) {
            (Some(name), Some((kind, whole))) => reading.symbols.push(Symbol {
                name: said(name),
                kind: kind.to_owned(),
                line: whole.start_position().row as i64 + 1,
                end_line: whole.end_position().row as i64 + 1,
            }),
            (Some(name), None) if reference => mention(&mut reading, name, said(name)),
            _ => {}
        }
        if let Some(import) = import {
            for name in imported {
                let between = if import.ends_with(language.separator) { "" } else { language.separator };
                reading.imports.insert(format!("{import}{between}{name}"));
            }
            reading.imports.insert(import);
        }
    }

    if component {
        let file = path.rsplit('/').next().unwrap_or(path);
        reading.symbols.push(Symbol {
            name: file.split('.').next().unwrap_or(file).to_owned(),
            kind: "component".into(),
            line: 1,
            end_line: reading.lines,
        });
        let tags = markup.as_deref().unwrap_or(text).split('<').skip(1);
        let names = tags.map(|tag| tag.split(|c: char| !c.is_alphanumeric()).next().unwrap_or(""));
        let components = names.filter(|name| name.starts_with(char::is_uppercase));
        reading.mentions.extend(components.map(|name| (name.to_owned(), false)));
    }
    let mut seen = HashSet::new();
    reading.symbols.retain(|symbol| seen.insert((symbol.name.clone(), symbol.kind.clone(), symbol.line)));
    let defined: HashSet<&str> = reading.symbols.iter().map(|s| s.name.as_str()).collect();
    reading.mentions.retain(|(name, _)| !defined.contains(name.as_str()));
    reading
}

fn line(import: &str) -> (Option<String>, Vec<String>) {
    let import = import.trim_start_matches("import").trim();
    let (from, names) = import.split_once('{').unwrap_or((import, ""));
    let names = names.trim_end_matches('}').split(',').filter_map(|name| name.split_whitespace().next());
    let from = from.trim_end_matches('.').to_owned();
    (Some(from), names.map(str::to_owned).collect())
}

fn mention(reading: &mut Reading, node: Node, name: String) {
    let member = node.prev_sibling().is_some_and(|before| MEMBER.contains(&before.kind()));
    reading.mentions.insert((name, member));
}

pub fn tree(path: &str, text: &str) -> String {
    let mut parser = Parser::new();
    let language = language(path).expect("a language we read");
    parser.set_language(&language.grammar).expect("a grammar that loads");
    parser.parse(text, None).expect("a tree").root_node().to_sexp()
}
