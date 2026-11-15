use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{MapArgs, Secret};

/// From a hook that an agent the core started runs, to that core over its socket, under
/// `map.read`: the code map of `folder`, in the workspace of the run whose MCP token it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MapQuery {
    /// The run's token for the core's MCP.
    pub token: Secret,
    pub folder: String,
    pub query: MapQueryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MapQueryKind {
    /// Where to begin reading the folder's code, as the MCP's `map` tool answers.
    Map(MapArgs),
    /// What a session starts with, from a core that offers `map.session_start`: the flows
    /// pinned by the docs that best match the title of the task of the run's chat, then the
    /// folder's map, both within `budget`; the map alone for a chat without a task.
    SessionStart(MapArgs),
    /// A query this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// The core's answer to a [`MapQuery`], under its envelope id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MapAnswer {
    /// None when the core has no map to give for that token and folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "String")]
    pub text: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Message;
    use crate::test_support::{envelope, never_sent, travels};

    #[test]
    fn a_query_and_its_answers_travel_as_tagged_json() {
        let query = envelope(Message::MapQuery(MapQuery {
            token: "t0k3n".into(),
            folder: "/work/app".to_owned(),
            query: MapQueryKind::Map(MapArgs { budget: Some(4000) }),
        }));
        let found = envelope(Message::MapAnswer(MapAnswer {
            text: Some("Start here.\n".to_owned()),
        }));
        let none = envelope(Message::MapAnswer(MapAnswer { text: None }));
        let start = envelope(Message::MapQuery(MapQuery {
            token: "t".into(),
            folder: "/work".to_owned(),
            query: MapQueryKind::SessionStart(MapArgs { budget: Some(4000) }),
        }));

        let query_wire = json!({ "id": 4, "type": "map_query", "token": "t0k3n",
            "folder": "/work/app", "query": { "kind": "map", "budget": 4000 } });
        travels(&query, query_wire);
        let found_wire = json!({ "id": 4, "type": "map_answer", "text": "Start here.\n" });
        travels(&found, found_wire);
        travels(&none, json!({ "id": 4, "type": "map_answer" }));
        let start_wire = json!({ "id": 4, "type": "map_query", "token": "t",
            "folder": "/work", "query": { "kind": "session_start", "budget": 4000 } });
        travels(&start, start_wire);
        assert!(!format!("{query:?}").contains("t0k3n"));
    }

    #[test]
    fn a_newer_query_parses_and_is_never_sent() {
        let wire = json!({ "id": 4, "type": "map_query", "token": "t",
            "folder": "/work", "query": { "kind": "flows" } });

        let Message::MapQuery(query) = never_sent(wire).message else {
            panic!("not a map query");
        };
        assert_eq!(query.query, MapQueryKind::Newer);
    }
}
