//! A sign-in terminal: the vendor's login command on a person's machine, shown in their
//! browser and sealed end to end with Noise NK, so the server relays bytes it can't read.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::token::token_id;
use crate::{AgentKind, Asker, Code, FrameError, MachineId, SignInState, WorkspaceId, to_hex};

token_id!(
    /// One sign-in terminal, chosen by the browser that opens it; 1–64 ASCII letters, digits,
    /// `-` or `_`.
    TerminalId,
    "not a terminal id"
);

/// The Noise protocol every sign-in terminal speaks; the browser initiates, and the core's
/// static key is its paired [`crate::MachineKey`].
pub const TERMINAL_NOISE: &str = "Noise_NK_25519_AESGCM_SHA256";

/// The largest Noise message, and so the most [`Bytes`] carries.
pub const NOISE_MAX: usize = 65535;

/// The largest plaintext one sealed frame holds: a Noise message less its tag.
pub const SEALED_MAX: usize = NOISE_MAX - 16;

/// From the owner of `machine`'s browser to its core, which offers `terminal.sign_in`. The
/// server relays it only from the machine's owner, and the core acts only when `by` is its
/// owner and refuses a terminal id it has already seen.
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
    /// Runs `agent`'s login, with the first handshake message, whose payload is a
    /// [`TerminalInput::Resize`].
    Open { agent: AgentKind, handshake: Bytes },
    /// A sealed [`TerminalInput`].
    Frame { sealed: Bytes },
    /// Ends the login, if it still runs.
    Close,
    /// An action this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From a core that offers `terminal.sign_in`, through the server, to the browser that opened
/// `terminal`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalUpdate {
    pub terminal: TerminalId,
    pub workspace: WorkspaceId,
    pub event: TerminalEvent,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalEvent {
    /// The second handshake message, with an empty payload.
    Accepted { handshake: Bytes },
    /// A sealed [`TerminalOutput`].
    Frame { sealed: Bytes },
    /// The terminal is gone; nothing more comes. `code` says why, unless it ended as asked.
    Closed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[schemars(with = "Code")]
        code: Option<Code>,
    },
    /// An event this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From the browser to the core, only inside the seal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalInput {
    /// What the person typed or pasted.
    Input { data: Bytes },
    /// The terminal's size in characters.
    Resize { cols: u32, rows: u32 },
    /// Input this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

/// From the core to the browser, only inside the seal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalOutput {
    /// What the login printed.
    Output { data: Bytes },
    /// The login ended, and then the vendor's status command said this.
    Ended { sign_in: SignInState },
    /// Output this build doesn't know, from a newer peer; only ever received.
    #[serde(other)]
    #[schemars(skip)]
    Newer,
}

impl TerminalInput {
    /// The plaintext to seal.
    ///
    /// # Errors
    /// For [`Self::Newer`], which is only ever received, or beyond [`SEALED_MAX`].
    pub fn to_json(&self) -> Result<Vec<u8>, FrameError> {
        sealed_json(self, matches!(self, Self::Newer))
    }

    /// # Errors
    /// When `json` isn't a [`TerminalInput`].
    pub fn from_json(json: &[u8]) -> Result<Self, FrameError> {
        serde_json::from_slice(json).map_err(|_| FrameError::Invalid)
    }
}

impl TerminalOutput {
    /// The plaintext to seal.
    ///
    /// # Errors
    /// For anything only ever received, or beyond [`SEALED_MAX`].
    pub fn to_json(&self) -> Result<Vec<u8>, FrameError> {
        let newer = matches!(
            self,
            Self::Newer
                | Self::Ended {
                    sign_in: SignInState::Newer
                }
        );
        sealed_json(self, newer)
    }

    /// # Errors
    /// When `json` isn't a [`TerminalOutput`].
    pub fn from_json(json: &[u8]) -> Result<Self, FrameError> {
        serde_json::from_slice(json).map_err(|_| FrameError::Invalid)
    }
}

fn sealed_json(value: &impl Serialize, newer: bool) -> Result<Vec<u8>, FrameError> {
    if newer {
        return Err(FrameError::Invalid);
    }
    let json = serde_json::to_vec(value).map_err(|_| FrameError::Invalid)?;
    if json.len() > SEALED_MAX {
        return Err(FrameError::TooLarge);
    }
    Ok(json)
}

impl TerminalCommand {
    pub(crate) const fn is_newer(&self) -> bool {
        matches!(
            self.action,
            TerminalAction::Newer
                | TerminalAction::Open {
                    agent: AgentKind::Newer,
                    ..
                }
        )
    }

    /// The Noise prologue both ends hash before the handshake of an [`TerminalAction::Open`],
    /// so the server can't move it to another machine, workspace, terminal or vendor. `None`
    /// for any other action, or an agent this build doesn't know.
    #[must_use]
    pub fn prologue(&self) -> Option<Vec<u8>> {
        let TerminalAction::Open { agent, .. } = self.action else {
            return None;
        };
        // Ids are tokens, which never hold the separator.
        let parts = [
            "weacon-terminal/1",
            self.machine.as_str(),
            self.workspace.as_str(),
            self.terminal.as_str(),
            agent.name()?,
        ];
        Some(parts.join("\n").into_bytes())
    }
}

impl TerminalUpdate {
    pub(crate) const fn is_newer(&self) -> bool {
        matches!(self.event, TerminalEvent::Newer)
    }
}

/// Bytes that travel as lowercase hex, at most [`NOISE_MAX`] of them. Its `Debug` shows only
/// how many, since they are sealed or terminal content.
#[derive(Clone, PartialEq, Eq, JsonSchema)]
pub struct Bytes(
    #[schemars(with = "String", regex(pattern = BYTES_PATTERN), length(max = HEX_MAX))] Vec<u8>,
);

// The schema's form of the check in `deserialize`; a test keeps the two in step.
const BYTES_PATTERN: &str = r"^([0-9a-f]{2})*$";
const HEX_MAX: usize = 2 * NOISE_MAX;

impl Bytes {
    /// `None` when there are more than [`NOISE_MAX`].
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Option<Self> {
        (bytes.len() <= NOISE_MAX).then_some(Self(bytes))
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
        let hex = String::deserialize(deserializer)?;
        let nibble = |digit: u8| match digit {
            b'0'..=b'9' => Some(digit - b'0'),
            b'a'..=b'f' => Some(digit - b'a' + 10),
            _ => None,
        };
        let pairs = hex.as_bytes().chunks(2);
        let bytes = pairs
            .map(|pair| match pair {
                [high, low] => Some(nibble(*high)? * 16 + nibble(*low)?),
                _ => None,
            })
            .collect::<Option<Vec<u8>>>()
            .ok_or_else(|| de::Error::custom("not lowercase hex bytes"))?;
        Self::new(bytes).ok_or_else(|| de::Error::custom("more bytes than a Noise message"))
    }
}

#[cfg(test)]
mod tests {
    use regex::Regex;
    use serde_json::{Value, json};

    use super::*;
    use crate::{AccountId, Envelope, MemberId, Message};

    fn bytes(raw: &[u8]) -> Bytes {
        Bytes::new(raw.to_vec()).unwrap()
    }

    fn command(action: TerminalAction) -> TerminalCommand {
        TerminalCommand {
            terminal: TerminalId::try_from("t-1".to_owned()).unwrap(),
            machine: MachineId::try_from("m-1".to_owned()).unwrap(),
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            action,
            by: None,
        }
    }

    fn update(event: TerminalEvent) -> Envelope {
        Envelope {
            id: 3,
            message: Message::Terminal(TerminalUpdate {
                terminal: TerminalId::try_from("t-1".to_owned()).unwrap(),
                workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
                event,
            }),
        }
    }

    fn travels(envelope: &Envelope, wire: &Value) {
        assert_eq!(serde_json::to_value(envelope).unwrap(), *wire);
        assert_eq!(
            Envelope::from_json(&envelope.to_json().unwrap()).unwrap(),
            *envelope
        );
    }

    #[test]
    fn commands_have_a_flat_tagged_shape() {
        let open = TerminalCommand {
            by: Some(Asker {
                account: AccountId::try_from("a-1".to_owned()).unwrap(),
                member: MemberId::try_from("p-1".to_owned()).unwrap(),
            }),
            ..command(TerminalAction::Open {
                agent: AgentKind::Codex,
                handshake: bytes(&[0, 0xab, 0xff]),
            })
        };
        let ids = json!({ "id": 3, "type": "terminal_command", "terminal": "t-1",
            "machine": "m-1", "workspace": "w-1" });
        let with = |action: Value, extra: Value| {
            let mut wire = ids.clone();
            wire["action"] = action;
            if let Value::Object(extra) = extra {
                wire.as_object_mut().unwrap().extend(extra);
            }
            wire
        };
        let cases = [
            (
                open,
                with(
                    json!({ "kind": "open", "agent": "codex", "handshake": "00abff" }),
                    json!({ "by": { "account": "a-1", "member": "p-1" } }),
                ),
            ),
            (
                command(TerminalAction::Frame { sealed: bytes(b"") }),
                with(json!({ "kind": "frame", "sealed": "" }), json!(null)),
            ),
            (
                command(TerminalAction::Close),
                with(json!({ "kind": "close" }), json!(null)),
            ),
        ];
        for (command, wire) in cases {
            let envelope = Envelope {
                id: 3,
                message: Message::TerminalCommand(command),
            };
            travels(&envelope, &wire);
        }
    }

    #[test]
    fn updates_have_a_flat_tagged_shape() {
        let ids = json!({ "id": 3, "type": "terminal", "terminal": "t-1", "workspace": "w-1" });
        let events = [
            (
                TerminalEvent::Accepted {
                    handshake: bytes(&[1, 2]),
                },
                json!({ "kind": "accepted", "handshake": "0102" }),
            ),
            (
                TerminalEvent::Frame {
                    sealed: bytes(&[0x10]),
                },
                json!({ "kind": "frame", "sealed": "10" }),
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
            travels(&update(event), &wire);
        }
    }

    #[test]
    fn what_a_newer_peer_sends_is_received_but_never_sent() {
        let received = [
            json!({ "id": 3, "type": "terminal_command", "terminal": "t-1", "machine": "m-1",
                "workspace": "w-1", "action": { "kind": "rewind" } }),
            json!({ "id": 3, "type": "terminal_command", "terminal": "t-1", "machine": "m-1",
                "workspace": "w-1", "action": { "kind": "open", "agent": "amp",
                "handshake": "" } }),
            json!({ "id": 3, "type": "terminal", "terminal": "t-1", "workspace": "w-1",
                "event": { "kind": "paused" } }),
        ];
        for wire in received {
            let envelope: Envelope = serde_json::from_value(wire.clone()).unwrap();
            assert_eq!(envelope.to_json(), Err(FrameError::Invalid), "{wire}");
        }
        assert_eq!(
            TerminalInput::from_json(br#"{"kind":"paste"}"#).unwrap(),
            TerminalInput::Newer
        );
        TerminalInput::Newer.to_json().unwrap_err();
        let outputs = [
            r#"{"kind":"bell"}"#,
            r#"{"kind":"ended","sign_in":"locked"}"#,
        ];
        for json in outputs {
            let output = TerminalOutput::from_json(json.as_bytes()).unwrap();
            assert_eq!(output.to_json(), Err(FrameError::Invalid), "{json}");
        }
    }

    #[test]
    fn what_travels_inside_the_seal_has_a_tagged_shape() {
        let inputs = [
            (
                TerminalInput::Input {
                    data: bytes(b"y\r"),
                },
                r#"{"kind":"input","data":"790d"}"#,
            ),
            (
                TerminalInput::Resize { cols: 80, rows: 24 },
                r#"{"kind":"resize","cols":80,"rows":24}"#,
            ),
        ];
        for (input, json) in inputs {
            assert_eq!(input.to_json().unwrap(), json.as_bytes());
            assert_eq!(TerminalInput::from_json(json.as_bytes()).unwrap(), input);
        }
        let outputs = [
            (
                TerminalOutput::Output { data: bytes(b"ok") },
                r#"{"kind":"output","data":"6f6b"}"#,
            ),
            (
                TerminalOutput::Ended {
                    sign_in: SignInState::SignedIn,
                },
                r#"{"kind":"ended","sign_in":"signed_in"}"#,
            ),
            (
                TerminalOutput::Ended {
                    sign_in: SignInState::Unknown,
                },
                r#"{"kind":"ended","sign_in":"unknown"}"#,
            ),
        ];
        for (output, json) in outputs {
            assert_eq!(output.to_json().unwrap(), json.as_bytes());
            assert_eq!(TerminalOutput::from_json(json.as_bytes()).unwrap(), output);
        }
        TerminalInput::from_json(b"not json").unwrap_err();

        TerminalOutput::from_json(br#"{"kind":"output"}"#).unwrap_err();
    }

    #[test]
    fn a_plaintext_that_wouldnt_fit_one_sealed_frame_is_refused() {
        // Each byte of data takes two in the JSON, around which the rest takes 26 or 27.
        let fits = 32_746;
        let data = |len| bytes(&vec![7; len]);
        let output = |len| TerminalOutput::Output { data: data(len) }.to_json();
        let input = |len| TerminalInput::Input { data: data(len) }.to_json();
        assert_eq!(output(fits).unwrap().len(), 65_519);
        assert_eq!(input(fits).unwrap().len(), 65_518);
        assert_eq!(output(fits + 1), Err(FrameError::TooLarge));
        assert_eq!(input(fits + 1), Err(FrameError::TooLarge));
    }

    #[test]
    fn bytes_are_lowercase_hex_no_longer_than_a_noise_message_and_match_the_schema() {
        let pattern = Regex::new(BYTES_PATTERN).unwrap();
        let parse = |hex: &str| serde_json::from_value::<Bytes>(json!(hex));
        let full = "ff".repeat(NOISE_MAX);
        for hex in ["", "00", "0123456789abcdef", full.as_str()] {
            let parsed = parse(hex).unwrap();
            assert!(pattern.is_match(hex), "{hex}");
            assert_eq!(serde_json::to_value(&parsed).unwrap(), json!(hex));
        }
        assert_eq!(parse("0a1f").unwrap().as_slice(), [0x0a, 0x1f]);
        for hex in ["0", "0A", "0g", "zz", " 00", "000"] {
            parse(hex).unwrap_err();
            assert!(!pattern.is_match(hex), "{hex}");
        }
        parse(&format!("{full}00")).unwrap_err();
        assert!(Bytes::new(vec![0; NOISE_MAX]).is_some());
        assert!(Bytes::new(vec![0; NOISE_MAX + 1]).is_none());
        serde_json::from_value::<Bytes>(json!(7)).unwrap_err();
    }

    #[test]
    fn bytes_never_show_in_debug() {
        let secret = bytes(b"MARKER-CODE-1234");
        let shown = format!(
            "{:?}",
            command(TerminalAction::Frame {
                sealed: secret.clone()
            })
        );
        assert!(shown.contains("Bytes(16)"), "{shown}");
        assert!(!shown.contains("4d41"), "{shown}");
        assert!(!format!("{:?}", TerminalInput::Input { data: secret }).contains("MARKER"));
    }

    #[test]
    fn the_prologue_binds_the_machine_workspace_terminal_and_vendor() {
        let open = |agent| {
            command(TerminalAction::Open {
                agent,
                handshake: bytes(b""),
            })
        };
        assert_eq!(
            open(AgentKind::Claude).prologue().unwrap(),
            b"weacon-terminal/1\nm-1\nw-1\nt-1\nclaude"
        );
        assert_eq!(
            open(AgentKind::Codex).prologue().unwrap(),
            b"weacon-terminal/1\nm-1\nw-1\nt-1\ncodex"
        );
        assert_eq!(open(AgentKind::Newer).prologue(), None);
        assert_eq!(command(TerminalAction::Close).prologue(), None);
        let other = TerminalCommand {
            workspace: WorkspaceId::try_from("w-2".to_owned()).unwrap(),
            ..open(AgentKind::Claude)
        };
        assert_ne!(other.prologue(), open(AgentKind::Claude).prologue());
    }
}
