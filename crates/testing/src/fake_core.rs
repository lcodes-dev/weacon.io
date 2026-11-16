//! The hello a test's client sends, and a fake core's answer to it.

use weacon_protocol::{App, Capability, Code, Envelope, Hello, Message, code};

use crate::Failure;

const RUN_STREAM: Capability = Capability::new(code!("run.stream"));

/// The answer to the client's first `line`, offering `run.stream` and `more`. A client it
/// welcomes must offer `run.stream`.
pub(crate) fn answer(line: &str, more: &[Capability]) -> Result<Envelope, Failure> {
    let envelope = Envelope::from_json(line.trim().as_bytes())?;
    let Message::Hello(hello) = envelope.message else {
        return Err(format!("expected a hello but got {}", line.trim()).into());
    };
    let core = App {
        name: "weacon-core".to_owned(),
        version: "0.0.0".to_owned(),
        commit: "0000000".to_owned(),
    };
    let capabilities = [&[RUN_STREAM], more].concat();
    let message = hello.answer(core, capabilities);
    if matches!(message, Message::Welcome(_)) && !hello.capabilities.contains(&RUN_STREAM) {
        return Err("the client didn't offer run.stream".into());
    }
    Ok(Envelope {
        id: envelope.id,
        message,
    })
}

/// The hello a test's client sends first, under `id`, speaking `protocol` and offering
/// `capabilities`.
pub fn client_hello(
    id: u32,
    protocol: u32,
    capabilities: impl IntoIterator<Item = Code>,
) -> Envelope {
    Envelope {
        id,
        message: Message::Hello(Hello {
            protocol,
            app: App {
                name: "weacon-web".to_owned(),
                version: "0.1.0".to_owned(),
                commit: "1a2b3c4".to_owned(),
            },
            capabilities: capabilities.into_iter().map(Capability::new).collect(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{PROTOCOL, encode_line};

    use super::*;

    fn hello(id: u32, protocol: u32, capabilities: impl IntoIterator<Item = Code>) -> String {
        String::from_utf8(encode_line(&client_hello(id, protocol, capabilities)).unwrap()).unwrap()
    }

    #[test]
    fn a_hello_is_welcomed_with_its_id() {
        let answer = answer(&hello(7, PROTOCOL, [code!("run.stream")]), &[]).unwrap();

        assert_eq!(answer.id, 7);
        let Message::Welcome(welcome) = answer.message else {
            panic!("not a welcome");
        };
        assert_eq!(welcome.capabilities, [RUN_STREAM]);
    }

    #[test]
    fn a_client_that_cant_take_run_updates_is_refused() {
        let refused = answer(&hello(1, PROTOCOL, [code!("chat.send")]), &[]).unwrap_err();

        assert_eq!(refused.to_string(), "the client didn't offer run.stream");
    }

    #[test]
    fn anything_but_a_hello_first_is_refused() {
        let line = r#"{"id":1,"type":"error","code":"store.full","retryable":false}"#;

        let refused = answer(line, &[]).unwrap_err();

        assert!(refused.to_string().starts_with("expected a hello"));
    }

    #[test]
    fn another_protocol_is_answered_with_an_error() {
        let answer = answer(&hello(1, PROTOCOL + 1, []), &[]).unwrap();

        let Message::Error(error) = answer.message else {
            panic!("not an error");
        };
        assert_eq!(error.code, code!("protocol.unsupported"));
    }
}
