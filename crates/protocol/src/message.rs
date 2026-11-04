use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Code, code};

/// The protocol version this build speaks; peers must match it exactly.
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
}

/// The answer to a [`Hello`] that speaks the same protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Welcome {
    pub app: App,
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
    /// The answer from a peer running `app`: a [`Welcome`] on the same protocol, or the error
    /// `protocol.unsupported`.
    #[must_use]
    pub fn answer(&self, app: App) -> Message {
        if self.protocol == PROTOCOL {
            Message::Welcome(Welcome { app })
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
        }
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
                    }),
                },
                json!({ "id": 1, "type": "welcome",
                        "app": { "name": "weacon-core", "version": "0.1.0", "commit": "1a2b3c4" } }),
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

        assert_eq!(unknown.message, Message::Unknown);
        assert!(matches!(extra.message, Message::Error(_)));
    }

    #[test]
    fn a_matching_hello_is_welcomed_and_any_other_refused() {
        assert_eq!(
            hello(PROTOCOL).answer(app("weacon-core")),
            Message::Welcome(Welcome {
                app: app("weacon-core"),
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
