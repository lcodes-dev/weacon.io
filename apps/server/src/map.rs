use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Display;
use std::io;
use std::io::Read as _;
use std::sync::LazyLock;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::db::{self, User, Waiting};
use crate::teams::membership;
use crate::{ApiError, AppState, github};

/// Raise this when what `read` finds changes, such as with a new language: every map is then read again.
const READER: i64 = 5;

/// The languages whose files are components: see `scripts`.
const COMPONENTS: [&str; 2] = ["Vue", "Svelte"];

/// Larger files are listed in the map and not read.
const MAX_BYTES: usize = 500_000;

/// How much new text one mapping may keep in memory before the commit is refused.
const MAX_NEW_BYTES: usize = 200_000_000;

/// How much an archive may unpack to, counting the files that are skipped.
const MAX_UNPACKED: u64 = 2_000_000_000;

pub struct Language {
    pub name: &'static str,
    extensions: &'static [&'static str],
    tags: TagsConfiguration,
}

impl Language {
    fn new(
        name: &'static str,
        extensions: &'static [&'static str],
        grammar: impl Into<tree_sitter::Language>,
        tags: &str,
    ) -> Self {
        let tags = TagsConfiguration::new(grammar.into(), tags, "")
            .unwrap_or_else(|error| panic!("{name}'s tags query doesn't fit: {error}"));
        Self {
            name,
            extensions,
            tags,
        }
    }
}

/// Adding a language is one grammar in Cargo.toml and one line here.
/// A grammar that ships no tags query has ours, in `assets/tags/`.
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
    let jsx = include_str!("../assets/tags/jsx.scm");
    let javascript_tags = format!("{}\n{jsx}", javascript::TAGS_QUERY);
    // TypeScript's tags query only adds to JavaScript's, and C++'s to C's.
    let typescript_tags = format!("{}\n{}", javascript::TAGS_QUERY, typescript::TAGS_QUERY);
    let tsx_tags = format!("{typescript_tags}\n{jsx}");
    let cpp_tags = format!("{}\n{}", c::TAGS_QUERY, cpp::TAGS_QUERY);
    // C#'s query names one capture in a way the reader refuses.
    let c_sharp_tags = c_sharp::TAGS_QUERY.replace(") @module", ") @definition.module");
    let ts = typescript::LANGUAGE_TYPESCRIPT;
    let language = Language::new;
    vec![
        language(
            "JavaScript",
            &["js", "jsx", "mjs", "cjs"],
            javascript::LANGUAGE,
            &javascript_tags,
        ),
        language("TypeScript", &["ts", "mts", "cts"], ts, &typescript_tags),
        language("TypeScript", &["tsx"], typescript::LANGUAGE_TSX, &tsx_tags),
        // Only the scripts of these two are read, as TypeScript: see `scripts`.
        language("Vue", &["vue"], ts, &typescript_tags),
        language("Svelte", &["svelte"], ts, &typescript_tags),
        language("Rust", &["rs"], rust::LANGUAGE, rust::TAGS_QUERY),
        language("Python", &["py"], python::LANGUAGE, python::TAGS_QUERY),
        language("PHP", &["php"], php::LANGUAGE_PHP, php::TAGS_QUERY),
        language("Go", &["go"], go::LANGUAGE, go::TAGS_QUERY),
        language("Java", &["java"], java::LANGUAGE, java::TAGS_QUERY),
        // A header named `.h` may be C++, and is read as C all the same.
        language("C", &["c", "h"], c::LANGUAGE, c::TAGS_QUERY),
        language(
            "C++",
            &["cpp", "cc", "cxx", "hpp", "hh", "hxx"],
            cpp::LANGUAGE,
            &cpp_tags,
        ),
        language("C#", &["cs"], c_sharp::LANGUAGE, &c_sharp_tags),
        language("Ruby", &["rb"], ruby::LANGUAGE, ruby::TAGS_QUERY),
        language("Swift", &["swift"], swift::LANGUAGE, swift::TAGS_QUERY),
        language("Dart", &["dart"], dart::LANGUAGE, dart::TAGS_QUERY),
        language(
            "Elixir",
            &["ex", "exs"],
            elixir::LANGUAGE,
            elixir::TAGS_QUERY,
        ),
        language("Lua", &["lua"], lua::LANGUAGE, lua::TAGS_QUERY),
        language(
            "Kotlin",
            &["kt", "kts"],
            kotlin::LANGUAGE,
            include_str!("../assets/tags/kotlin.scm"),
        ),
        language(
            "Scala",
            &["scala", "sc"],
            scala::LANGUAGE,
            include_str!("../assets/tags/scala.scm"),
        ),
        language(
            "Bash",
            &["sh", "bash"],
            bash::LANGUAGE,
            include_str!("../assets/tags/bash.scm"),
        ),
        language(
            "Zig",
            &["zig"],
            zig::LANGUAGE,
            include_str!("../assets/tags/zig.scm"),
        ),
    ]
});

#[derive(Serialize)]
pub struct Symbol {
    pub name: String,
    /// Such as `function`, `method` or `class`.
    pub kind: String,
    pub line: i64,
    pub end_line: i64,
}

/// What the map keeps about one content.
#[derive(Default)]
pub struct Reading {
    pub lines: i64,
    pub symbols: Vec<Symbol>,
    /// The names it mentions without defining them.
    pub mentions: BTreeSet<String>,
    /// The content itself, when it is text.
    pub text: Option<String>,
}

/// The language a file is read as, by its name.
pub fn language(path: &str) -> Option<&'static Language> {
    let extension = path.rsplit('.').next().unwrap_or_default();
    LANGUAGES
        .iter()
        .find(|language| language.extensions.contains(&extension))
}

/// Blanks that keep every line and position of what follows where it was.
fn blank(text: &str) -> impl Iterator<Item = char> {
    text.bytes()
        .map(|byte| if byte == b'\n' { '\n' } else { ' ' })
}

/// A Vue or Svelte file with its comments blanked.
fn uncommented(text: &str) -> String {
    let mut kept = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("<!--") {
        let end = rest[at..]
            .find("-->")
            .map_or(rest.len(), |end| at + end + 3);
        kept.push_str(&rest[..at]);
        kept.extend(blank(&rest[at..end]));
        rest = &rest[end..];
    }
    kept + rest
}

/// What a content is kept under: git's id of it, with what of the file's name changes how it is read.
fn key(path: &str, blob: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match language(path) {
        Some(language) if COMPONENTS.contains(&language.name) => format!("{blob} {name}"),
        Some(_) => format!("{blob} {}", name.rsplit('.').next().unwrap_or_default()),
        None => blob.to_owned(),
    }
}

/// A Vue or Svelte file with all but its scripts blanked.
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

fn read(path: &str, bytes: &[u8]) -> Reading {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Reading::default();
    };
    let mut reading = Reading {
        lines: text.lines().count() as i64,
        text: Some(text.to_owned()).filter(|text| !text.is_empty()),
        ..Reading::default()
    };
    let Some(language) = language(path) else {
        return reading;
    };
    let component = COMPONENTS.contains(&language.name);
    let markup = component.then(|| uncommented(text));
    let scripts = markup.as_deref().map(scripts);
    let source = scripts.as_deref().map_or(bytes, str::as_bytes);
    let mut context = TagsContext::new();
    let Ok((tags, _)) = context.generate_tags(&language.tags, source, None) else {
        return reading;
    };
    let line_ends: Vec<usize> = (0..bytes.len()).filter(|at| bytes[*at] == b'\n').collect();
    for tag in tags.flatten() {
        let name = text[tag.name_range].to_owned();
        if tag.is_definition {
            let ends_before = line_ends.partition_point(|at| *at < tag.range.end);
            reading.symbols.push(Symbol {
                name,
                kind: language.tags.syntax_type_name(tag.syntax_type_id).into(),
                line: tag.span.start.row as i64 + 1,
                end_line: ends_before as i64 + 1,
            });
        } else {
            reading.mentions.insert(name);
        }
    }
    if component {
        // The file is the component, named as other files write its tag.
        let file = path.rsplit('/').next().unwrap_or(path);
        reading.symbols.push(Symbol {
            name: file.split('.').next().unwrap_or(file).to_owned(),
            kind: "component".into(),
            line: 1,
            end_line: reading.lines,
        });
        let tags = markup.as_deref().unwrap_or(text).split('<').skip(1);
        let names = tags.map(|tag| {
            tag.split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or("")
        });
        let components = names.filter(|name| name.starts_with(char::is_uppercase));
        reading.mentions.extend(components.map(str::to_owned));
    }
    // Two patterns of a tags query can match one definition.
    let mut seen = HashSet::new();
    reading
        .symbols
        .retain(|symbol| seen.insert((symbol.name.clone(), symbol.kind.clone(), symbol.line)));
    let defined: HashSet<&str> = reading.symbols.iter().map(|s| s.name.as_str()).collect();
    reading
        .mentions
        .retain(|name| !defined.contains(name.as_str()));
    reading
}

/// Reads from a commit's archive the files whose content is new, by git's id of it.
fn read_archive(
    archive: &[u8],
    mut new: HashMap<String, String>,
) -> io::Result<Vec<(String, Reading)>> {
    let mut readings = Vec::new();
    let mut kept = 0;
    let mut archive = tar::Archive::new(GzDecoder::new(archive).take(MAX_UNPACKED));
    for entry in archive.entries()? {
        let mut entry = entry?;
        // Everything in GitHub's archive sits in one folder named after the commit.
        let path: std::path::PathBuf = entry.path()?.components().skip(1).collect();
        let path = path.to_string_lossy().into_owned();
        let Some(blob) = new.remove(&path) else {
            continue;
        };
        let mut bytes = Vec::new();
        if entry.size() as usize <= MAX_BYTES {
            entry.read_to_end(&mut bytes)?;
        }
        kept += bytes.len();
        if kept > MAX_NEW_BYTES {
            return Err(io::Error::other("it holds too much text to map yet"));
        }
        readings.push((blob, read(&path, &bytes)));
    }
    // Whatever the archive lacks, such as a link, is recorded so it isn't asked for again.
    readings.extend(new.into_values().map(|blob| (blob, Reading::default())));
    Ok(readings)
}

/// Runs for as long as the server does.
pub async fn worker(state: AppState) {
    if let Err(error) = db::forget_maps_of_other_readers(&state.pool, READER).await {
        tracing::error!("forgetting old maps failed: {error}");
    }
    loop {
        round(&state).await;
        tokio::select! {
            () = tokio::time::sleep(Duration::from_secs(60)) => {}
            () = state.map_wake.notified() => {}
        }
    }
}

/// Notes each tracked branch's latest commit, then maps the branches that are behind.
pub async fn round(state: &AppState) {
    if let Err(error) = notice(state).await {
        tracing::error!("noticing commits failed: {error}");
    }
    let waiting = db::waiting_branches(&state.pool).await.unwrap_or_default();
    for branch in waiting {
        if let Err(ApiError(_, message)) = map(state, &branch).await {
            tracing::error!("mapping {} {} failed: {message}", branch.repo, branch.name);
            let (project, name) = (branch.project_id, &branch.name);
            let saved =
                db::set_branch_head(&state.pool, &project, name, None, Some(&message)).await;
            saved.ok();
        }
    }
}

async fn notice(state: &AppState) -> sqlx::Result<()> {
    for project in db::tracked_projects(&state.pool).await? {
        let heads = match github::token(state, &project.team_id).await {
            Ok(token) => github::branches(state, &token, &project.repo).await,
            Err(error) => Err(error),
        };
        for name in db::branch_names(&state.pool, &project.id).await? {
            let (head, error) = match &heads {
                Ok(heads) => match heads.get(&name) {
                    Some(head) => (Some(head.as_str()), None),
                    None => (None, Some("This branch isn't on GitHub")),
                },
                Err(ApiError(_, message)) => (None, Some(message.as_str())),
            };
            db::set_branch_head(&state.pool, &project.id, &name, head, error).await?;
        }
    }
    Ok(())
}

async fn map(state: &AppState, branch: &Waiting) -> Result<(), ApiError> {
    let token = github::token(state, &branch.team_id).await?;
    let files = github::files(state, &token, &branch.repo, &branch.head_sha).await?;
    let files: Vec<(String, String)> = files
        .into_iter()
        .map(|(path, blob)| {
            let key = key(&path, &blob);
            (path, key)
        })
        .collect();
    let mut seen = db::known_blobs(&state.pool, &branch.project_id).await?;
    // One path for each content not read before, even when several files share it.
    let new: HashMap<String, String> = files
        .iter()
        .filter(|(_, blob)| seen.insert(blob.clone()))
        .cloned()
        .collect();
    let mut readings = Vec::new();
    if !new.is_empty() {
        let archive = github::archive(state, &token, &branch.repo, &branch.head_sha).await?;
        // Parsing is slow work that must not hold up the server's other requests.
        let reading = tokio::task::spawn_blocking(move || read_archive(&archive, new));
        // A partial reading would be saved as the whole map, so any failure stops here.
        readings = reading.await.map_err(unread)?.map_err(unread)?;
    }
    db::save_map(&state.pool, branch, &files, &readings).await?;
    Ok(())
}

fn unread(error: impl Display) -> ApiError {
    let message = format!("The commit's archive couldn't be read: {error}");
    ApiError(StatusCode::BAD_GATEWAY, message)
}

#[derive(Deserialize)]
pub struct Which {
    branch: String,
}

pub async fn show(
    State(state): State<AppState>,
    user: User,
    Path((team_id, project_id)): Path<(String, String)>,
    Query(which): Query<Which>,
) -> Result<Json<db::Map>, ApiError> {
    membership(&user, &team_id)?;
    let map = db::map(&state.pool, &team_id, &project_id, &which.branch).await?;
    Ok(Json(map.ok_or_else(crate::not_found)?))
}
