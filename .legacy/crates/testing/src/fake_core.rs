//! The hello a test's client sends, and a fake core's answer to it.

use weacon_protocol::{App, Envelope, Hello, Message};

use crate::Failure;

/// The answer to the client's first `line`.
pub(crate) fn answer(line: &str) -> Result<Envelope, Failure> {
    let envelope = Envelope::from_json(line.trim().as_bytes())?;
    let Message::Hello(hello) = envelope.message else {
        return Err(format!("expected a hello but got {}", line.trim()).into());
    };
    let core = App {
        name: "weacon-core".to_owned(),
        version: "0.0.0".to_owned(),
        commit: "0000000".to_owned(),
    };
    Ok(Envelope {
        id: envelope.id,
        message: hello.answer(core),
    })
}

/// The hello a test's client sends first, under `id`, speaking `protocol`.
#[must_use]
pub fn client_hello(id: u32, protocol: u32) -> Envelope {
    Envelope {
        id,
        message: Message::Hello(Hello {
            protocol,
            app: App {
                name: "weacon-web".to_owned(),
                version: "0.1.0".to_owned(),
                commit: "1a2b3c4".to_owned(),
            },
            capabilities: Vec::new(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{PROTOCOL, code, encode_line};

    use super::*;

    fn hello(id: u32, protocol: u32) -> String {
        String::from_utf8(encode_line(&client_hello(id, protocol)).unwrap()).unwrap()
    }

    #[test]
    fn a_hello_is_welcomed_with_its_id() {
        let answer = answer(&hello(7, PROTOCOL)).unwrap();

        assert_eq!(answer.id, 7);
        assert!(matches!(answer.message, Message::Welcome(_)));
    }

    #[test]
    fn anything_but_a_hello_first_is_refused() {
        let line = r#"{"id":1,"type":"error","code":"store.full","retryable":false}"#;

        let refused = answer(line).unwrap_err();

        assert!(refused.to_string().starts_with("expected a hello"));
    }

    #[test]
    fn another_protocol_is_answered_with_an_error() {
        let answer = answer(&hello(1, PROTOCOL + 1)).unwrap();

        let Message::Error(error) = answer.message else {
            panic!("not an error");
        };
        assert_eq!(error.code, code!("protocol.unsupported"));
    }
}
