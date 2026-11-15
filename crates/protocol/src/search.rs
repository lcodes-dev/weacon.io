use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ChatId, DocId, TaskId};

/// Asks the server, under the capability `workspace.search`, for the tasks, docs and chats in
/// the workspace that hold every word of `text`, at most 8 words of at most 64 characters, in
/// 256 bytes. Words of three or more characters match whatever their case and accents; shorter
/// ones match as typed, ignoring only ASCII case, and text without a longer word finds nothing.
/// `limit` is 20 without one, and at most 50.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchQuery {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "u32")]
    pub limit: Option<u32>,
}

/// The server's answer to a [`SearchQuery`]: matches in the title first, then the most
/// recently changed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchAnswer {
    pub hits: Vec<SearchHit>,
}

/// One thing a [`SearchQuery`] found, with its title and where it matched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SearchHit {
    Task {
        task: TaskId,
        title: String,
        snippet: Snippet,
    },
    Doc {
        doc: DocId,
        title: String,
        snippet: Snippet,
    },
    /// A chat, by its title or by one of the messages its log keeps: a person's prompt or an
    /// agent's text. `seq` is that message's place in the log.
    Chat {
        chat: ChatId,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "u64")]
        seq: Option<u64>,
        snippet: Snippet,
    },
    /// A hit this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// Plain text from around a match, and the parts of it that matched.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Snippet {
    pub text: String,
    pub marks: Vec<Mark>,
}

/// The part of a [`Snippet`] from `start` up to `end`, counted in UTF-16 code units as a
/// browser counts a string.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Mark {
    pub start: u32,
    pub end: u32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn hits_are_tagged_by_kind_and_a_newer_one_still_parses() {
        let hit = SearchHit::Doc {
            doc: DocId::try_from("d1".to_owned()).unwrap(),
            title: "Plan".to_owned(),
            snippet: Snippet {
                text: "the plan".to_owned(),
                marks: vec![Mark { start: 4, end: 8 }],
            },
        };
        let wire = json!({
            "kind": "doc",
            "doc": "d1",
            "title": "Plan",
            "snippet": { "text": "the plan", "marks": [{ "start": 4, "end": 8 }] },
        });

        assert_eq!(serde_json::to_value(&hit).unwrap(), wire);
        assert_eq!(serde_json::from_value::<SearchHit>(wire).unwrap(), hit);
        let chat = SearchHit::Chat {
            chat: ChatId::try_from("c1".to_owned()).unwrap(),
            title: "Fix the build".to_owned(),
            seq: Some(3),
            snippet: Snippet::default(),
        };
        let chat_wire = json!({
            "kind": "chat",
            "chat": "c1",
            "title": "Fix the build",
            "seq": 3,
            "snippet": { "text": "", "marks": [] },
        });
        assert_eq!(serde_json::to_value(&chat).unwrap(), chat_wire);
        assert_eq!(
            serde_json::from_value::<SearchHit>(chat_wire).unwrap(),
            chat
        );
        let newer = json!({ "kind": "symbol", "name": "a::b" });
        assert_eq!(
            serde_json::from_value::<SearchHit>(newer).unwrap(),
            SearchHit::Newer
        );
    }

    #[test]
    fn a_query_needs_only_its_text() {
        let query: SearchQuery = serde_json::from_value(json!({ "text": "plan" })).unwrap();

        assert_eq!(
            query,
            SearchQuery {
                text: "plan".to_owned(),
                limit: None
            }
        );
        assert_eq!(
            serde_json::to_value(&query).unwrap(),
            json!({ "text": "plan" })
        );
    }
}
