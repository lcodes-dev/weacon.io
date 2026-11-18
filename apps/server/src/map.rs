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
use sqlx::FromRow;
use tree_sitter_tags::{TagsConfiguration, TagsContext};

use crate::db::{self, User, Waiting};
use crate::teams::membership;
use crate::{ApiError, AppState, github};

/// Raise this when what `read` finds changes, such as with a new language: every map is then read again.
const READER: i64 = 2;

/// Larger files are listed in the map and not read.
const MAX_BYTES: usize = 500_000;

/// How much new text one mapping may keep in memory before the commit is refused.
const MAX_NEW_BYTES: usize = 200_000_000;

pub struct Language {
    pub name: &'static str,
    extensions: &'static [&'static str],
    tags: TagsConfiguration,
}

/// Adding a language is one grammar in Cargo.toml and one line here.
static LANGUAGES: LazyLock<Vec<Language>> = LazyLock::new(|| {
    use tree_sitter_javascript as javascript;
    use tree_sitter_typescript as typescript;
    // TypeScript's tags query only adds to JavaScript's.
    let typescript_tags = format!("{}\n{}", javascript::TAGS_QUERY, typescript::TAGS_QUERY);
    let language = |name, extensions, grammar: tree_sitter::Language, tags: &str| Language {
        name,
        extensions,
        tags: TagsConfiguration::new(grammar, tags, "").expect("a grammar's own tags query"),
    };
    vec![
        language(
            "JavaScript",
            &["js", "jsx", "mjs", "cjs"],
            javascript::LANGUAGE.into(),
            javascript::TAGS_QUERY,
        ),
        language(
            "TypeScript",
            &["ts", "mts", "cts"],
            typescript::LANGUAGE_TYPESCRIPT.into(),
            &typescript_tags,
        ),
        language(
            "TypeScript",
            &["tsx"],
            typescript::LANGUAGE_TSX.into(),
            &typescript_tags,
        ),
        language(
            "Rust",
            &["rs"],
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::TAGS_QUERY,
        ),
        language(
            "Python",
            &["py"],
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::TAGS_QUERY,
        ),
        language(
            "PHP",
            &["php"],
            tree_sitter_php::LANGUAGE_PHP.into(),
            tree_sitter_php::TAGS_QUERY,
        ),
    ]
});

#[derive(Serialize, FromRow)]
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
    let mut context = TagsContext::new();
    let Ok((tags, _)) = context.generate_tags(&language.tags, bytes, None) else {
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
    // Two patterns of a tags query can match one definition.
    reading
        .symbols
        .dedup_by(|a, b| a.name == b.name && a.kind == b.kind && a.line == b.line);
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
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
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
            let saved = db::set_branch_head(&state.pool, project, name, None, Some(&message)).await;
            saved.ok();
        }
    }
}

async fn notice(state: &AppState) -> sqlx::Result<()> {
    for project in db::tracked_projects(&state.pool).await? {
        let heads = match github::token(state, project.team_id).await {
            Ok(token) => github::branches(state, &token, &project.repo).await,
            Err(error) => Err(error),
        };
        for name in db::branch_names(&state.pool, project.id).await? {
            let (head, error) = match &heads {
                Ok(heads) => match heads.get(&name) {
                    Some(head) => (Some(head.as_str()), None),
                    None => (None, Some("This branch isn't on GitHub")),
                },
                Err(ApiError(_, message)) => (None, Some(message.as_str())),
            };
            db::set_branch_head(&state.pool, project.id, &name, head, error).await?;
        }
    }
    Ok(())
}

async fn map(state: &AppState, branch: &Waiting) -> Result<(), ApiError> {
    let token = github::token(state, branch.team_id).await?;
    let files = github::files(state, &token, &branch.repo, &branch.head_sha).await?;
    let mut seen = db::known_blobs(&state.pool, branch.project_id).await?;
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
    Path((team_id, project_id)): Path<(i64, i64)>,
    Query(which): Query<Which>,
) -> Result<Json<db::Map>, ApiError> {
    membership(&user, team_id)?;
    let map = db::map(&state.pool, team_id, project_id, &which.branch).await?;
    Ok(Json(map.ok_or_else(crate::not_found)?))
}
