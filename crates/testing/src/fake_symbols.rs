//! Made-up items of a repository's code, which a fake answers a symbols query with.

use weacon_protocol::{
    Envelope, Message, RepoId, Symbol, SymbolKind, SymbolsAnswer, SymbolsAnswerKind,
    SymbolsQueryKind,
};

/// The made-up items: kind, name, file and line.
const ITEMS: [(SymbolKind, &str, &str, u32); 4] = [
    (SymbolKind::Struct, "garden::Bed", "src/bed.rs", 3),
    (SymbolKind::Fn, "garden::Bed::water", "src/bed.rs", 12),
    (SymbolKind::Fn, "garden::plant", "src/lib.rs", 8),
    (SymbolKind::Enum, "garden::Season", "src/season.rs", 1),
];

/// The answer to the symbols query in `envelope`, under its id: the items whose name holds every
/// word, ignoring case; none for any other message.
pub(crate) fn answer(envelope: &Envelope) -> Option<Envelope> {
    let Message::SymbolsQuery(query) = &envelope.message else {
        return None;
    };
    let SymbolsQueryKind::Search { words } = &query.query;
    let words = words.to_lowercase();
    let repo: RepoId = "garden".parse().ok()?;
    let symbols = ITEMS
        .into_iter()
        .filter(|(_, name, ..)| {
            let name = name.to_lowercase();
            words.split_whitespace().all(|word| name.contains(word))
        })
        .map(|(kind, name, path, line)| Symbol {
            repo: repo.clone(),
            kind,
            name: name.to_owned(),
            path: path.to_owned(),
            line,
        })
        .collect();
    Some(Envelope {
        id: envelope.id,
        message: Message::SymbolsAnswer(SymbolsAnswer {
            request: query.request.clone(),
            machine: query.machine.clone(),
            workspace: query.workspace.clone(),
            answer: SymbolsAnswerKind::Found {
                symbols,
                more: false,
            },
        }),
    })
}

#[cfg(test)]
mod tests {
    use weacon_protocol::SymbolsQuery;

    use super::*;

    fn search(words: &str) -> Envelope {
        Envelope {
            id: 5,
            message: Message::SymbolsQuery(SymbolsQuery {
                request: "r-1".parse().unwrap(),
                machine: "mc-1".parse().unwrap(),
                workspace: "w-1".parse().unwrap(),
                query: SymbolsQueryKind::Search {
                    words: words.to_owned(),
                },
                by: None,
            }),
        }
    }

    fn names(answer: Option<Envelope>) -> Vec<String> {
        let Some(Envelope {
            id: 5,
            message:
                Message::SymbolsAnswer(SymbolsAnswer {
                    answer: SymbolsAnswerKind::Found { symbols, .. },
                    ..
                }),
        }) = answer
        else {
            panic!("{answer:?}");
        };
        symbols.into_iter().map(|symbol| symbol.name).collect()
    }

    #[test]
    fn finds_the_items_whose_name_holds_every_word() {
        assert_eq!(
            names(answer(&search("BED"))),
            ["garden::Bed", "garden::Bed::water"]
        );
        assert_eq!(names(answer(&search("bed water"))), ["garden::Bed::water"]);
        assert!(names(answer(&search("rose"))).is_empty());
        let other = Envelope {
            id: 5,
            message: Message::Unknown,
        };
        assert_eq!(answer(&other), None);
    }
}
