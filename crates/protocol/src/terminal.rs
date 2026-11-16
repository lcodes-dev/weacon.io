//! A sign-in terminal: the vendor's login command on a person's machine, shown in their
//! browser. The server relays it and never parses, stores or logs what it carries.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::hex::{from_hex, to_hex};
use crate::token::token_id;
use crate::{AgentKind, Asker, Code, MachineId, WorkspaceId};

token_id!(
    /// One sign-in terminal, chosen by the browser that opens it; 1–64 ASCII letters, digits,
    /// `-` or `_`.
    TerminalId,
    "not a terminal id"
);

/// A sign-in terminal's input, from the owner of `machine`'s browser to its core, which offers
/// `terminal.sign_in`. The server relays it only when it offers `terminal.relay`, and only
/// from the machine's owner, and the core acts only when `by` is its owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalCommand {
    pub terminal: TerminalId,
    pub machine: MachineId,
    pub workspace: WorkspaceId,
    pub action: TerminalAction,
    /// Who asked, when the server relays the message to a machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Asker")]
    pub by: Option<Asker>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalAction {
    /// Runs `agent`'s login in a terminal of this size, in characters.
    Open {
        agent: AgentKind,
        cols: u32,
        rows: u32,
    },
    /// What the person typed or pasted.
    Input { data: Bytes },
    /// Ends the login, if it still runs.
    Close,
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// A sign-in terminal's output, from a core that offers `terminal.sign_in`, through the
/// server, to the browser that opened `terminal`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalUpdate {
    pub terminal: TerminalId,
    pub workspace: WorkspaceId,
    pub event: TerminalEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalEvent {
    /// The login runs.
    Opened,
    /// What the login printed.
    Output { data: Bytes },
    /// The terminal is gone; nothing more comes. `code` says why, unless it ended as asked.
    Closed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "Code")]
        code: Option<Code>,
    },
    /// An event this build doesn't know, from a newer peer; only ever received.
    #[serde(other, skip_serializing)]
    #[schemars(skip)]
    Newer,
}

/// Bytes that travel as lowercase hex. Its `Debug` shows only how many, since they are
/// terminal content.
#[derive(Clone, PartialEq, Eq, JsonSchema)]
pub struct Bytes(#[schemars(with = "String", regex(pattern = BYTES_PATTERN))] Vec<u8>);

// The schema's form of the check in `deserialize`; a test keeps the two in step.
const BYTES_PATTERN: &str = r"^([0-9a-f]{2})*$";

impl Bytes {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bytes({})", self.0.len())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&to_hex(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        from_hex(&String::deserialize(deserializer)?)
            .map(Self)
            .ok_or_else(|| de::Error::custom("not lowercase hex bytes"))
    }
}

#[cfg(test)]
mod tests {
    use regex::Regex;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_support::{envelope, never_sent, travels, workspace};
    use crate::{Envelope, Message};

    fn bytes(raw: &[u8]) -> Bytes {
        Bytes::new(raw.to_vec())
    }

    fn command(action: TerminalAction) -> TerminalCommand {
        TerminalCommand {
            terminal: "t-1".parse().unwrap(),
            machine: "m-1".parse().unwrap(),
            workspace: workspace(),
            action,
            by: None,
        }
    }

    fn update(event: TerminalEvent) -> Envelope {
        envelope(Message::Terminal(TerminalUpdate {
            terminal: "t-1".parse().unwrap(),
            workspace: workspace(),
            event,
        }))
    }

    fn command_wire(action: &Value) -> Value {
        json!({ "id": 4, "type": "terminal_command", "terminal": "t-1", "machine": "m-1",
            "workspace": "w-1", "action": action })
    }

    #[test]
    fn commands_have_a_flat_tagged_shape() {
        let cases = [
            (
                TerminalAction::Input {
                    data: bytes(b"y\r"),
                },
                json!({ "kind": "input", "data": "790d" }),
            ),
            (TerminalAction::Close, json!({ "kind": "close" })),
        ];
        for (action, wire) in cases {
            let sent = envelope(Message::TerminalCommand(command(action)));
            travels(&sent, command_wire(&wire));
        }
    }

    #[test]
    fn a_relayed_open_names_who_asked() {
        let open = TerminalCommand {
            by: Some(Asker {
                account: "a-1".parse().unwrap(),
                member: "p-1".parse().unwrap(),
            }),
            ..command(TerminalAction::Open {
                agent: AgentKind::Codex,
                cols: 80,
                rows: 24,
            })
        };
        let mut wire =
            command_wire(&json!({ "kind": "open", "agent": "codex", "cols": 80, "rows": 24 }));
        wire["by"] = json!({ "account": "a-1", "member": "p-1" });

        travels(&envelope(Message::TerminalCommand(open)), wire);
    }

    #[test]
    fn updates_have_a_flat_tagged_shape() {
        let ids = json!({ "id": 4, "type": "terminal", "terminal": "t-1", "workspace": "w-1" });
        let events = [
            (TerminalEvent::Opened, json!({ "kind": "opened" })),
            (
                TerminalEvent::Output { data: bytes(b"ok") },
                json!({ "kind": "output", "data": "6f6b" }),
            ),
            (
                TerminalEvent::Closed { code: None },
                json!({ "kind": "closed" }),
            ),
            (
                TerminalEvent::Closed {
                    code: Some(crate::code!("terminal.busy")),
                },
                json!({ "kind": "closed", "code": "terminal.busy" }),
            ),
        ];
        for (event, body) in events {
            let mut wire = ids.clone();
            wire["event"] = body;
            travels(&update(event), wire);
        }
    }

    #[test]
    fn what_a_newer_peer_sends_is_received_but_never_sent() {
        let received = [
            json!({ "id": 4, "type": "terminal_command", "terminal": "t-1", "machine": "m-1",
                "workspace": "w-1", "action": { "kind": "rewind" } }),
            json!({ "id": 4, "type": "terminal_command", "terminal": "t-1", "machine": "m-1",
                "workspace": "w-1", "action": { "kind": "open", "agent": "amp",
                "cols": 80, "rows": 24 } }),
            json!({ "id": 4, "type": "terminal", "terminal": "t-1", "workspace": "w-1",
                "event": { "kind": "paused" } }),
        ];
        for wire in received {
            never_sent(wire);
        }
    }

    #[test]
    fn bytes_are_lowercase_hex_and_match_the_schema() {
        let pattern = Regex::new(BYTES_PATTERN).unwrap();
        let parse = |hex: &str| serde_json::from_value::<Bytes>(json!(hex));
        for hex in ["", "00", "0123456789abcdef"] {
            let parsed = parse(hex).unwrap();
            assert!(pattern.is_match(hex), "{hex}");
            assert_eq!(serde_json::to_value(&parsed).unwrap(), json!(hex));
        }
        assert_eq!(parse("0a1f").unwrap().as_slice(), [0x0a, 0x1f]);
        for hex in ["0", "0A", "0g", "zz", " 00", "000"] {
            parse(hex).unwrap_err();
            assert!(!pattern.is_match(hex), "{hex}");
        }
        serde_json::from_value::<Bytes>(json!(7)).unwrap_err();
    }

    #[test]
    fn bytes_never_show_in_debug() {
        let secret = bytes(b"MARKER-CODE-1234");
        let typed = command(TerminalAction::Input {
            data: secret.clone(),
        });
        let shown = format!("{typed:?}{:?}", TerminalEvent::Output { data: secret });
        assert!(shown.contains("Bytes(16)"), "{shown}");
        assert!(
            !shown.contains("4d41") && !shown.contains("MARKER"),
            "{shown}"
        );
    }
}
