use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

use weacon_observability::Fault;
use weacon_protocol::{App, Decoder, Envelope, Hello, Message, PROTOCOL, code, encode};

use crate::app::state_unavailable;
use crate::daemon::{build, socket_path};
use crate::platform;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Shakes hands with the daemon and prints its build.
pub(crate) fn run(app: &crate::app::App, out: &mut impl Write) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let daemon = query(&socket_path(state_dir), build(&app.version), TIMEOUT)?;
    writeln!(
        out,
        "Daemon: running, weacon {} ({})",
        daemon.version, daemon.commit
    )
    .map_err(|error| {
        Fault::new(
            code!("status.output_failed"),
            "The status couldn't be printed",
        )
        .because(error)
    })
}

/// The daemon's build, after a handshake as `me` over the socket at `socket`.
pub(crate) fn query(socket: &Path, me: App, timeout: Duration) -> Result<App, Fault> {
    let no_answer = |error: io::Error| {
        Fault::new(code!("daemon.no_answer"), "The daemon didn't answer").because(error)
    };
    let mut stream = platform::connect(socket).map_err(|error| {
        if matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ) {
            Fault::new(
                code!("daemon.not_running"),
                "The daemon isn't running; start it with weacon daemon",
            )
        } else {
            no_answer(error)
        }
    })?;
    stream.set_read_timeout(Some(timeout)).map_err(no_answer)?;
    stream.set_write_timeout(Some(timeout)).map_err(no_answer)?;
    let hello = Envelope {
        id: 1,
        message: Message::Hello(Hello {
            protocol: PROTOCOL,
            app: me,
        }),
    };
    let line = encode(&hello).map_err(|error| no_answer(io::Error::other(error)))?;
    stream.write_all(&line).map_err(no_answer)?;
    let mut decoder = Decoder::default();
    let mut buf = [0; 4096];
    loop {
        let read = stream.read(&mut buf).map_err(no_answer)?;
        if read == 0 {
            return Err(no_answer(io::ErrorKind::UnexpectedEof.into()));
        }
        let replies = decoder.push(buf.get(..read).unwrap_or_default());
        if let Some(reply) = replies.into_iter().find_map(Result::ok) {
            return welcomed(reply.message);
        }
    }
}

fn welcomed(message: Message) -> Result<App, Fault> {
    match message {
        Message::Welcome(welcome) => Ok(welcome.app),
        Message::Error(body) if body.code == code!("protocol.unsupported") => Err(Fault::new(
            code!("daemon.other_protocol"),
            "The daemon speaks another protocol version; restart it",
        )),
        Message::Hello(_) | Message::Error(_) | Message::Unknown => Err(Fault::new(
            code!("daemon.refused"),
            "The daemon refused the handshake",
        )),
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{ErrorBody, Welcome};

    use super::*;

    fn app() -> App {
        App {
            name: "weacon-core".to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        }
    }

    #[test]
    fn a_welcome_gives_the_daemons_build() {
        assert_eq!(
            welcomed(Message::Welcome(Welcome { app: app() })).unwrap(),
            app()
        );
    }

    #[test]
    fn a_refusal_says_why() {
        let refusal = |code| {
            welcomed(Message::Error(ErrorBody {
                code,
                retryable: false,
            }))
            .unwrap_err()
        };

        assert_eq!(
            refusal(code!("protocol.unsupported")).code().as_str(),
            "daemon.other_protocol"
        );
        assert_eq!(
            refusal(code!("handshake.required")).code().as_str(),
            "daemon.refused"
        );
        assert_eq!(
            welcomed(Message::Unknown).unwrap_err().code().as_str(),
            "daemon.refused"
        );
    }

    #[test]
    fn no_socket_means_no_daemon() {
        let error = query(Path::new("/nonexistent/weacon.sock"), app(), TIMEOUT).unwrap_err();

        assert_eq!(error.code().as_str(), "daemon.not_running");
    }
}
