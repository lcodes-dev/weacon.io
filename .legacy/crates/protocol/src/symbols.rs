use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Asker, MachineId, RepoId, RequestId, WorkspaceId};

/// A person asks their own machine for the items of the code in the repositories it has for a
/// workspace, from a client to a core. Neither the query nor its answer is stored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SymbolsQuery {
    pub request: RequestId,
    pub machine: MachineId,
    pub workspace: WorkspaceId,
    pub query: SymbolsQueryKind,
    /// Who asked, when the server relays the message to a machine; the core answers only its
    /// owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Asker")]
    pub by: Option<Asker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SymbolsQueryKind {
    /// The items whose path, or a contract they declare, holds every word, or a word's start,
    /// best first.
    Search { words: String },
}

/// The core's answer to a [`SymbolsQuery`] under the same `request`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SymbolsAnswer {
    pub request: RequestId,
    pub machine: MachineId,
    pub workspace: WorkspaceId,
    pub answer: SymbolsAnswerKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SymbolsAnswerKind {
    /// The best of each repository in turn; `more` when some were left out.
    Found { symbols: Vec<Symbol>, more: bool },
}

/// An item of a repository's code: where it is, never what it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Symbol {
    pub repo: RepoId,
    pub kind: SymbolKind,
    /// Its path in the code, such as `weacon_protocol::message::Envelope`.
    pub name: String,
    /// Its file, from the repository's root, joined with `/`.
    pub path: String,
    /// The line it begins at, counting from 1.
    pub line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Const,
    Enum,
    Fn,
    Macro,
    Mod,
    Static,
    Struct,
    Trait,
    Type,
    Union,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Message;
    use crate::test_support::{envelope, request, travels, workspace};

    fn machine() -> MachineId {
        "mc-1".parse().unwrap()
    }

    #[test]
    fn a_query_and_its_answer_travel_as_tagged_json() {
        let query = envelope(Message::SymbolsQuery(SymbolsQuery {
            request: request(),
            machine: machine(),
            workspace: workspace(),
            query: SymbolsQueryKind::Search {
                words: "envelope json".to_owned(),
            },
            by: None,
        }));
        let answer = envelope(Message::SymbolsAnswer(SymbolsAnswer {
            request: request(),
            machine: machine(),
            workspace: workspace(),
            answer: SymbolsAnswerKind::Found {
                symbols: vec![Symbol {
                    repo: "site".parse().unwrap(),
                    kind: SymbolKind::Fn,
                    name: "a::to_json".to_owned(),
                    path: "src/frame.rs".to_owned(),
                    line: 42,
                }],
                more: true,
            },
        }));

        let ids = json!({"id": 4, "request": "r-1", "machine": "mc-1", "workspace": "w-1"});
        let mut wire = ids.clone();
        wire["type"] = json!("symbols_query");
        wire["query"] = json!({"kind": "search", "words": "envelope json"});
        travels(&query, wire);
        let mut wire = ids;
        wire["type"] = json!("symbols_answer");
        wire["answer"] = json!({"kind": "found", "more": true, "symbols": [{
            "repo": "site", "kind": "fn", "name": "a::to_json", "path": "src/frame.rs", "line": 42,
        }]});
        travels(&answer, wire);
    }
}
