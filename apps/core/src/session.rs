use weacon_protocol::{App, Code, Envelope, ErrorBody, FrameError, Message, code};

/// One client's place in the conversation with the daemon.
#[derive(Debug, Default)]
pub(crate) struct Session {
    greeted: bool,
}

/// What the daemon sends back, and whether it then hangs up.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub(crate) envelope: Option<Envelope>,
    pub(crate) close: bool,
}

impl Reply {
    const fn error(id: u32, code: Code, close: bool) -> Self {
        Self {
            envelope: Some(Envelope {
                id,
                message: Message::Error(ErrorBody {
                    code,
                    retryable: false,
                }),
            }),
            close,
        }
    }
}

impl Session {
    pub(crate) const fn greeted(&self) -> bool {
        self.greeted
    }

    /// The reply to one received frame; `app` is the daemon's own build.
    pub(crate) fn answer(&mut self, received: Result<Envelope, FrameError>, app: &App) -> Reply {
        let envelope = match received {
            Ok(envelope) => envelope,
            Err(error) => return Reply::error(0, error.code(), false),
        };
        let id = envelope.id;
        match (self.greeted, envelope.message) {
            (false, Message::Hello(hello)) => {
                let message = hello.answer(app.clone());
                self.greeted = matches!(message, Message::Welcome(_));
                Reply {
                    envelope: Some(Envelope { id, message }),
                    close: !self.greeted,
                }
            }
            (false, _) => Reply::error(id, code!("handshake.required"), true),
            (true, Message::Error(_)) => Reply {
                envelope: None,
                close: false,
            },
            (true, Message::Hello(_)) => Reply::error(id, code!("handshake.repeated"), false),
            (true, _) => Reply::error(id, code!("message.unsupported"), false),
        }
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Hello, PROTOCOL, Welcome};

    use super::*;

    fn app(name: &str) -> App {
        App {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        }
    }

    fn hello(id: u32, protocol: u32) -> Envelope {
        Envelope {
            id,
            message: Message::Hello(Hello {
                protocol,
                app: app("weacon-desktop"),
            }),
        }
    }

    fn error_code(reply: &Reply) -> Option<&str> {
        match &reply.envelope.as_ref()?.message {
            Message::Error(body) => Some(body.code.as_str()),
            Message::Hello(_) | Message::Welcome(_) | Message::Unknown => None,
        }
    }

    #[test]
    fn a_matching_hello_is_welcomed_once() {
        let mut session = Session::default();
        let welcome = session.answer(Ok(hello(1, PROTOCOL)), &app("weacon-core"));
        let again = session.answer(Ok(hello(2, PROTOCOL)), &app("weacon-core"));

        assert_eq!(
            welcome,
            Reply {
                envelope: Some(Envelope {
                    id: 1,
                    message: Message::Welcome(Welcome {
                        app: app("weacon-core")
                    }),
                }),
                close: false,
            }
        );
        assert!(session.greeted());
        assert_eq!(error_code(&again), Some("handshake.repeated"));
        assert!(!again.close);
    }

    #[test]
    fn another_protocol_is_refused_and_hung_up_on() {
        let mut session = Session::default();
        let reply = session.answer(Ok(hello(1, PROTOCOL + 1)), &app("weacon-core"));

        assert_eq!(error_code(&reply), Some("protocol.unsupported"));
        assert!(reply.close);
        assert!(!session.greeted());
    }

    #[test]
    fn anything_before_the_hello_is_refused_and_hung_up_on() {
        let mut session = Session::default();
        let reply = session.answer(
            Ok(Envelope {
                id: 4,
                message: Message::Unknown,
            }),
            &app("weacon-core"),
        );

        assert_eq!(reply.envelope.as_ref().map(|envelope| envelope.id), Some(4));
        assert_eq!(error_code(&reply), Some("handshake.required"));
        assert!(reply.close);
    }

    #[test]
    fn after_the_hello_unknown_messages_get_an_error_and_errors_get_nothing() {
        let mut session = Session::default();
        session.answer(Ok(hello(1, PROTOCOL)), &app("weacon-core"));
        let unknown = session.answer(
            Ok(Envelope {
                id: 5,
                message: Message::Unknown,
            }),
            &app("weacon-core"),
        );
        let error = session.answer(
            Ok(Envelope {
                id: 6,
                message: Message::Error(ErrorBody {
                    code: code!("store.full"),
                    retryable: true,
                }),
            }),
            &app("weacon-core"),
        );

        assert_eq!(error_code(&unknown), Some("message.unsupported"));
        assert!(!unknown.close);
        assert_eq!(
            error,
            Reply {
                envelope: None,
                close: false
            }
        );
    }

    #[test]
    fn a_bad_frame_gets_its_error_and_the_connection_stays() {
        let mut session = Session::default();
        let reply = session.answer(Err(FrameError::TooLarge), &app("weacon-core"));

        assert_eq!(reply.envelope.as_ref().map(|envelope| envelope.id), Some(0));
        assert_eq!(error_code(&reply), Some("frame.too_large"));
        assert!(!reply.close);
    }
}
