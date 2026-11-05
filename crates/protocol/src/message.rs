use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Capability, Code, Receipt, code};

/// The protocol version this build speaks. Only a breaking change raises it; peers must match
/// it exactly and check each other's capabilities for everything else.
pub const PROTOCOL: u32 = 1;

/// One message on the wire. A reply carries the `id` of the request it answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Envelope {
    pub id: u32,
    #[serde(flatten)]
    pub message: Message,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    Hello(Hello),
    Welcome(Welcome),
    Error(ErrorBody),
    Receipt(Receipt),
    /// A message type this build doesn't know, from a newer peer.
    #[serde(other)]
    #[schemars(skip)]
    Unknown,
}

/// The first message on every connection, from the side that connects.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Hello {
    pub protocol: u32,
    pub app: App,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,
}

/// The answer to a [`Hello`] that speaks the same protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Welcome {
    pub app: App,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,
}

/// A program, such as `weacon-desktop`, and the exact build it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct App {
    pub name: String,
    pub version: String,
    pub commit: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: Code,
    /// Whether the same request can succeed if sent again later.
    pub retryable: bool,
}

impl Hello {
    /// A [`Welcome`] from `app` on the same protocol, else the error `protocol.unsupported`.
    #[must_use]
    pub fn answer(&self, app: App) -> Message {
        if self.protocol == PROTOCOL {
            Message::Welcome(Welcome {
                app,
                capabilities: Vec::new(),
            })
        } else {
            Message::Error(ErrorBody {
                code: code!("protocol.unsupported"),
                retryable: false,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn app(name: &str) -> App {
        App {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        }
    }

    fn hello(protocol: u32) -> Hello {
        Hello {
            protocol,
            app: app("weacon-desktop"),
            capabilities: Vec::new(),
        }
    }

    fn capabilities(names: &[&str]) -> Vec<Capability> {
        serde_json::from_value(json!(names)).unwrap()
    }

    #[test]
    fn messages_have_a_flat_tagged_shape() {
        let cases = [
            (
                Envelope {
                    id: 1,
                    message: Message::Hello(hello(PROTOCOL)),
                },
                json!({ "id": 1, "type": "hello", "protocol": 1,
                        "app": { "name": "weacon-desktop", "version": "0.1.0", "commit": "1a2b3c4" } }),
            ),
            (
                Envelope {
                    id: 1,
                    message: Message::Welcome(Welcome {
                        app: app("weacon-core"),
                        capabilities: capabilities(&["chat.send"]),
                    }),
                },
                json!({ "id": 1, "type": "welcome",
                        "app": { "name": "weacon-core", "version": "0.1.0", "commit": "1a2b3c4" },
                        "capabilities": ["chat.send"] }),
            ),
            (
                Envelope {
                    id: 7,
                    message: Message::Error(ErrorBody {
                        code: code!("store.full"),
                        retryable: true,
                    }),
                },
                json!({ "id": 7, "type": "error", "code": "store.full", "retryable": true }),
            ),
        ];

        for (envelope, wire) in cases {
            assert_eq!(serde_json::to_value(&envelope).unwrap(), wire);
            assert_eq!(serde_json::from_value::<Envelope>(wire).unwrap(), envelope);
        }
    }

    #[test]
    fn newer_peers_messages_and_fields_still_parse() {
        let unknown: Envelope =
            serde_json::from_value(json!({ "id": 3, "type": "chat.send", "text": "hi" })).unwrap();
        let extra: Envelope = serde_json::from_value(
            json!({ "id": 4, "type": "error", "code": "store.full", "retryable": false, "hint": 2 }),
        )
        .unwrap();

        let newer: Envelope =
            serde_json::from_value(json!({ "id": 5, "type": "hello", "protocol": 1,
            "app": { "name": "weacon-desktop", "version": "9.0.0", "commit": "1a2b3c4" },
            "capabilities": ["chat.send", "future.thing"] }))
            .unwrap();

        assert_eq!(unknown.message, Message::Unknown);
        assert!(matches!(extra.message, Message::Error(_)));
        assert!(
            matches!(newer.message, Message::Hello(hello) if hello.capabilities == capabilities(&["chat.send", "future.thing"]))
        );
    }

    #[test]
    fn a_matching_hello_is_welcomed_and_any_other_refused() {
        assert_eq!(
            hello(PROTOCOL).answer(app("weacon-core")),
            Message::Welcome(Welcome {
                app: app("weacon-core"),
                capabilities: Vec::new(),
            })
        );
        for protocol in [PROTOCOL - 1, PROTOCOL + 1] {
            assert_eq!(
                hello(protocol).answer(app("weacon-core")),
                Message::Error(ErrorBody {
                    code: code!("protocol.unsupported"),
                    retryable: false,
                })
            );
        }
    }
}
