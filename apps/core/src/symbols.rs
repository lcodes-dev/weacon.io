//! The items of the code that a person's web app asks their own machine for, in the
//! repositories it has for a workspace. Neither the words nor what they find is stored or
//! logged.

use weacon_observability::{Fault, Logger};
use weacon_protocol::{
    RepoId, Symbol, SymbolKind, SymbolsAnswer, SymbolsAnswerKind, SymbolsQuery, SymbolsQueryKind,
    code,
};

use crate::codemap::SEARCHED;
use crate::db::Hit;
use crate::mcp::Maps;

/// The most items an answer holds.
const MOST: usize = 50;
/// The longest words a search takes.
const LONGEST_WORDS: usize = 200;
/// The longest name or path an answer holds.
const LONGEST: usize = 500;

/// Answers `query` from the maps of the workspace's repositories; the caller checks who asks.
pub(crate) async fn query(
    maps: &Maps,
    query: SymbolsQuery,
    log: &Logger,
) -> Result<SymbolsAnswer, Fault> {
    let SymbolsQueryKind::Search { words } = query.query else {
        return Err(Fault::new(
            code!("symbols.unknown"),
            "This core doesn't know that request",
        ));
    };
    if words.len() > LONGEST_WORDS {
        return Err(Fault::new(
            code!("symbols.too_long"),
            "The words are too long to search for",
        ));
    }
    let found = maps.symbols(&query.workspace, &words, log).await?;
    let (symbols, more) = best(found);
    Ok(SymbolsAnswer {
        request: query.request,
        machine: query.machine,
        workspace: query.workspace,
        answer: SymbolsAnswerKind::Found { symbols, more },
    })
}

/// The best of each repository's hits in turn, at most [`MOST`], and whether any were left out,
/// a repository's own search among them when it found as many as it returns.
fn best(found: Vec<(RepoId, Vec<Hit>)>) -> (Vec<Symbol>, bool) {
    let cut = found
        .iter()
        .any(|(_, hits)| u32::try_from(hits.len()).unwrap_or(u32::MAX) >= SEARCHED);
    let mut each: Vec<_> = found
        .into_iter()
        .map(|(repo, hits)| hits.into_iter().filter_map(move |hit| symbol(&repo, hit)))
        .collect();
    let mut symbols = Vec::new();
    loop {
        let before = symbols.len();
        symbols.extend(each.iter_mut().filter_map(Iterator::next));
        if symbols.len() == before || symbols.len() > MOST {
            break;
        }
    }
    let more = cut || symbols.len() > MOST;
    symbols.truncate(MOST);
    (symbols, more)
}

/// `hit` as an answer holds it; none for a document, or a name or path too long to send.
fn symbol(repo: &RepoId, hit: Hit) -> Option<Symbol> {
    if hit.name.len() > LONGEST || hit.path.len() > LONGEST {
        return None;
    }
    let kind: SymbolKind = serde_json::from_value(hit.kind.into()).ok()?;
    (kind != SymbolKind::Newer).then(|| Symbol {
        repo: repo.clone(),
        kind,
        name: hit.name,
        path: hit.path,
        line: hit.line,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use weacon_protocol::{AccountId, Asker, MachineId, RequestId};
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::mcp::maps_support::{crate_repo, setup, workspace};

    fn id<T: TryFrom<String>>(id: &str) -> T
    where
        T::Error: std::fmt::Debug,
    {
        T::try_from(id.to_owned()).unwrap()
    }

    fn query(machine: &str, by: Option<&str>, words: &str) -> SymbolsQuery {
        SymbolsQuery {
            request: id::<RequestId>("r-1"),
            machine: id::<MachineId>(machine),
            workspace: workspace("one"),
            query: SymbolsQueryKind::Search {
                words: words.to_owned(),
            },
            by: by.map(|account| Asker {
                account: id::<AccountId>(account),
                member: id("m-1"),
            }),
        }
    }

    fn hit(kind: &str, name: &str) -> Hit {
        Hit {
            path: "src/lib.rs".to_owned(),
            line: 2,
            kind: kind.to_owned(),
            name: name.to_owned(),
        }
    }

    #[tokio::test]
    async fn the_owner_finds_the_items_of_the_workspaces_repositories() {
        let dir = TempDir::new().unwrap();
        let setup = setup(&dir).await;
        let main = crate_repo(&dir, "one");
        setup.register(&workspace("one"), "site", &main);
        fs::write(main.join("src/lib.rs"), "pub fn MARKER() {}\n").unwrap();
        let (log, records) = recording_logger();

        let answer = query_as(&setup.maps, query("mc-1", Some("a-1"), "run"), &log).await;

        let found = SymbolsAnswerKind::Found {
            symbols: vec![Symbol {
                repo: id("site"),
                kind: SymbolKind::Fn,
                name: "one::run".to_owned(),
                path: "src/lib.rs".to_owned(),
                line: 1,
            }],
            more: false,
        };
        let answer = answer.unwrap();
        assert_eq!(answer.answer, found);
        assert_eq!(
            (answer.request, answer.machine, answer.workspace),
            (id("r-1"), id("mc-1"), workspace("one"))
        );
        assert!(records.lock().unwrap().is_empty());
    }

    async fn query_as(
        maps: &Maps,
        query: SymbolsQuery,
        log: &Logger,
    ) -> Result<SymbolsAnswer, Fault> {
        super::query(maps, query, log).await
    }

    #[tokio::test]
    async fn only_known_words_of_a_fair_length_are_searched() {
        let (log, _records) = recording_logger();
        let maps = Maps::off();
        let too_long = "x".repeat(LONGEST_WORDS + 1);
        let longest = "x".repeat(LONGEST_WORDS);
        let mut newer = query("mc-1", Some("a-1"), "run");
        newer.query = SymbolsQueryKind::Newer;
        for (asked, refused) in [
            (newer, "symbols.unknown"),
            (query("mc-1", Some("a-1"), &too_long), "symbols.too_long"),
            (query("mc-1", Some("a-1"), &longest), "mcp.map_failed"),
        ] {
            let fault = query_as(&maps, asked, &log).await.unwrap_err();
            assert_eq!(fault.code().as_str(), refused);
        }
    }

    #[test]
    fn the_best_of_each_repository_comes_in_turn_up_to_the_most_an_answer_holds() {
        let (one, two): (RepoId, RepoId) = (id("one"), id("two"));
        let hits = |prefix: &str, count: usize| -> Vec<Hit> {
            (0..count)
                .map(|n| hit("fn", &format!("{prefix}{n}")))
                .collect()
        };
        let names = |symbols: &[Symbol]| -> Vec<String> {
            symbols.iter().map(|symbol| symbol.name.clone()).collect()
        };

        let (symbols, more) = best(vec![
            (one.clone(), hits("a", 3)),
            (two.clone(), hits("b", 1)),
        ]);
        assert_eq!(names(&symbols), ["a0", "b0", "a1", "a2"]);
        assert!(!more);
        assert_eq!(symbols[1].repo, two);
        let (symbols, more) = best(vec![(one.clone(), hits("a", MOST - 1))]);
        assert_eq!((symbols.len(), more), (MOST - 1, false));
        // A repository that found as many as it returns may have found more.
        let (symbols, more) = best(vec![(one.clone(), hits("a", MOST))]);
        assert_eq!((symbols.len(), more), (MOST, true));
        let (symbols, more) = best(vec![(one, hits("a", 30)), (two, hits("b", 30))]);
        assert_eq!((symbols.len(), more), (MOST, true));
        assert_eq!(names(&symbols).last().map(String::as_str), Some("b24"));
        assert_eq!(best(Vec::new()), (Vec::new(), false));
    }

    #[test]
    fn documents_unknown_kinds_and_names_or_paths_too_long_to_send_are_left_out() {
        let repo: RepoId = id("one");
        let mut long_path = hit("fn", "a::run");
        long_path.path = "p".repeat(LONGEST + 1);
        let mut longest_path = hit("struct", "a::Run");
        longest_path.path = "p".repeat(LONGEST);

        assert_eq!(symbol(&repo, hit("doc", "docs/a.md")), None);
        assert_eq!(symbol(&repo, hit("class", "A")), None);
        assert_eq!(symbol(&repo, hit("fn", &"n".repeat(LONGEST + 1))), None);
        assert!(symbol(&repo, hit("fn", &"n".repeat(LONGEST))).is_some());
        assert_eq!(symbol(&repo, long_path), None);
        let kept = symbol(&repo, longest_path).unwrap();
        assert_eq!((kept.kind, kept.line), (SymbolKind::Struct, 2));
    }
}
