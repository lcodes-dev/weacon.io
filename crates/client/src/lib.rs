//! Connects to the core's daemon over its socket and shakes hands with it.

mod platform;

use std::error::Error;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use weacon_observability::Fault;
use weacon_protocol::{App, Decoder, Envelope, Hello, Message, PROTOCOL, code, encode};

const TIMEOUT: Duration = Duration::from_secs(5);

/// The daemon's build, after a handshake as `me` over the socket at `socket`.
///
/// # Errors
/// When no daemon listens there, it doesn't answer in time, or it refuses the handshake.
pub fn handshake(socket: &Path, me: App) -> Result<App, Fault> {
    let mut stream = platform::connect(socket, TIMEOUT).map_err(|error| {
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
    let hello = Envelope {
        id: 1,
        message: Message::Hello(Hello {
            protocol: PROTOCOL,
            app: me,
        }),
    };
    let line = encode(&hello).map_err(no_answer)?;
    stream.write_all(&line).map_err(no_answer)?;
    let mut decoder = Decoder::default();
    let mut buf = [0; 4096];
    // One deadline for the whole answer, so a daemon sending only bad lines can't stall us.
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let left = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| no_answer(io::Error::from(io::ErrorKind::TimedOut)))?;
        stream.set_read_timeout(Some(left)).map_err(no_answer)?;
        let read = stream.read(&mut buf).map_err(no_answer)?;
        let Some(bytes) = buf.get(..read).filter(|bytes| !bytes.is_empty()) else {
            return Err(no_answer(io::Error::from(io::ErrorKind::UnexpectedEof)));
        };
        let replies = decoder.push(bytes);
        if let Some(reply) = replies.into_iter().find_map(Result::ok) {
            return welcomed(reply.message);
        }
    }
}

fn no_answer(error: impl Into<Box<dyn Error + Send + Sync>>) -> Fault {
    Fault::new(code!("daemon.no_answer"), "The daemon didn't answer").because(error)
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
    use std::fs;
    use std::io::{BufRead, BufReader};
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;

    use weacon_protocol::{ErrorBody, Welcome};

    use super::*;

    fn app(name: &str) -> App {
        App {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("weacon-client-{name}-{}", std::process::id()));
        drop(fs::remove_dir_all(&dir));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A one-connection daemon that reads the hello, then sends a bad frame and `reply`, or hangs up.
    fn serve_once(socket: &Path, reply: Option<Message>) -> thread::JoinHandle<Envelope> {
        let listener = UnixListener::bind(socket).unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(&stream).read_line(&mut line).unwrap();
            if let Some(message) = reply {
                stream.write_all(b"not json\n").unwrap();
                stream
                    .write_all(&encode(&Envelope { id: 1, message }).unwrap())
                    .unwrap();
            }
            serde_json::from_str(&line).unwrap()
        })
    }

    #[test]
    fn a_welcome_gives_the_daemons_build_after_our_hello() {
        let dir = temp_dir("welcome");
        let socket = dir.join("weacon.sock");
        let daemon = serve_once(
            &socket,
            Some(Message::Welcome(Welcome {
                app: app("weacon-core"),
            })),
        );

        assert_eq!(
            handshake(&socket, app("weacon-desktop")).unwrap(),
            app("weacon-core")
        );
        assert_eq!(
            daemon.join().unwrap(),
            Envelope {
                id: 1,
                message: Message::Hello(Hello {
                    protocol: PROTOCOL,
                    app: app("weacon-desktop"),
                }),
            }
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_daemon_that_hangs_up_gave_no_answer() {
        let dir = temp_dir("hangup");
        let socket = dir.join("weacon.sock");
        let daemon = serve_once(&socket, None);
        let error = handshake(&socket, app("weacon-desktop")).unwrap_err();
        drop(daemon.join());

        assert_eq!(error.code().as_str(), "daemon.no_answer");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_refusal_says_why() {
        let refusal = |code| {
            welcomed(Message::Error(ErrorBody {
                code,
                retryable: false,
            }))
            .unwrap_err()
            .code()
            .as_str()
            .to_owned()
        };

        assert_eq!(
            refusal(code!("protocol.unsupported")),
            "daemon.other_protocol"
        );
        assert_eq!(refusal(code!("handshake.required")), "daemon.refused");
        assert_eq!(
            welcomed(Message::Unknown).unwrap_err().code().as_str(),
            "daemon.refused"
        );
    }

    #[test]
    fn no_socket_or_a_dead_one_means_no_daemon() {
        let dir = temp_dir("dead");
        let socket = dir.join("weacon.sock");
        drop(UnixListener::bind(&socket).unwrap());

        for path in [socket.as_path(), Path::new("/nonexistent/weacon.sock")] {
            let error = handshake(path, app("weacon-desktop")).unwrap_err();

            assert_eq!(error.code().as_str(), "daemon.not_running");
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
