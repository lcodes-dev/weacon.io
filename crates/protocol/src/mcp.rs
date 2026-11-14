//! The tool calls of the MCP the core serves to its own runs. MCP's envelope is the vendor
//! protocol, which rmcp parses; a call's name and arguments are these types. Neither the web
//! app nor the extension reads them, so they stay out of `schema.json`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// From an agent the core started, to that core's MCP: one tool call, by name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case")]
pub enum McpCall {
    /// Where to begin reading the code of the run's folder.
    Map(MapArgs),
    /// One item of that code.
    Symbol(SymbolArgs),
    /// The flows the folder's docs pin, and where each breaks.
    Flow(FlowArgs),
    /// A tool this build doesn't offer; only ever received.
    #[serde(other)]
    Newer,
}

impl McpCall {
    /// The call to the tool `name` with MCP's `arguments`, which may be left out.
    ///
    /// # Errors
    /// When the arguments don't fit the tool.
    pub fn parse(name: &str, arguments: Option<Map<String, Value>>) -> serde_json::Result<Self> {
        let mut call = arguments.unwrap_or_default();
        call.insert("tool".into(), name.into());
        serde_json::from_value(Value::Object(call))
    }
}

/// From a run's agent, under `McpCall::Map`: how much of the map it wants.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapArgs {
    /// The most bytes the answer may hold; the core clamps it to its own bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u32>,
}

/// From a run's agent, under `McpCall::Flow`: how much of the flows it wants.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowArgs {
    /// The most bytes the answer may hold; the core clamps it to its own bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u32>,
}

/// From a run's agent, under `McpCall::Symbol`: the item it asks about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolArgs {
    /// The item's path, `weacon_protocol::frame::Envelope`.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<u32>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn call(name: &str, arguments: Value) -> serde_json::Result<McpCall> {
        let Value::Object(arguments) = arguments else {
            return McpCall::parse(name, None);
        };
        McpCall::parse(name, Some(arguments))
    }

    #[test]
    fn a_call_is_read_by_its_name_and_unknown_fields_are_ignored() {
        let map = call("map", json!({"budget": 2000, "later": 1, "tool": "symbol"}));
        let symbol = call("symbol", json!({"path": "a::b"}));

        assert_eq!(map.unwrap(), McpCall::Map(MapArgs { budget: Some(2000) }));
        assert_eq!(
            symbol.unwrap(),
            McpCall::Symbol(SymbolArgs {
                path: "a::b".into(),
                budget: None,
            })
        );
        let bare = McpCall::parse("map", None).unwrap();
        assert_eq!(bare, McpCall::Map(MapArgs::default()));
        let flow = call("flow", json!({"budget": 3000}));
        assert_eq!(
            flow.unwrap(),
            McpCall::Flow(FlowArgs { budget: Some(3000) })
        );
    }

    #[test]
    fn an_unknown_tool_is_newer_and_a_symbol_needs_its_path() {
        assert_eq!(call("later", json!({"path": 1})).unwrap(), McpCall::Newer);
        call("symbol", json!({})).unwrap_err();
        call("map", json!({"budget": "big"})).unwrap_err();
    }
}
