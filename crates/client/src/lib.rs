//! Connects to the core's daemon over its socket and shakes hands with it.

pub mod platform;

use std::collections::VecDeque;
use std::error::Error;
use std::io;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use weacon_observability::Fault;
use weacon_protocol::{
    App, Capability, Envelope, FrameError, Hello, LineDecoder, Message, PROTOCOL, Welcome, code,
    encode_line,
};

const TIMEOUT: Duration = Duration::from_secs(5);
const READ_CHUNK: usize = 8192;

/// The daemon's build and capabilities, after a handshake as `me` over the socket at `socket`.
///
/// # Errors
/// When no daemon listens there, it doesn't answer in time, or it refuses the handshake.
pub async fn handshake(socket: &Path, me: App) -> Result<Welcome, Fault> {
    Ok(Connection::open(socket, me, Vec::new()).await?.0)
}

/// A raw connection to the daemon at `socket`, before any handshake, for relaying another app's
/// frames.
///
/// # Errors
/// When no daemon listens there, or it doesn't answer in time.
pub async fn connect(socket: &Path) -> Result<impl AsyncRead + AsyncWrite + Unpin + use<>, Fault> {
    tokio::time::timeout(TIMEOUT, platform::connect(socket))
        .await
        .map_err(no_answer)?
        .map_err(not_running)
}

/// A connection to the daemon after a welcome, which yields what the daemon sends next.
pub struct Connection {
    stream: platform::Stream,
    decoder: LineDecoder,
    pending: VecDeque<Result<Envelope, FrameError>>,
}

impl Connection {
    /// Connects to the daemon at `socket` as `me`, offering `capabilities`, and shakes hands,
    /// giving the daemon's welcome.
    ///
    /// # Errors
    /// When no daemon listens there, it doesn't answer in time, or it refuses the handshake.
    pub async fn open(
        socket: &Path,
        me: App,
        capabilities: Vec<Capability>,
    ) -> Result<(Welcome, Self), Fault> {
        tokio::time::timeout(TIMEOUT, Self::greet(socket, me, capabilities))
            .await
            .unwrap_or_else(|_| Err(no_answer(io::Error::from(io::ErrorKind::TimedOut))))
    }

    async fn greet(
        socket: &Path,
        me: App,
        capabilities: Vec<Capability>,
    ) -> Result<(Welcome, Self), Fault> {
        let mut stream = platform::connect(socket).await.map_err(not_running)?;
        let hello = Envelope {
            id: 1,
            message: Message::Hello(Hello {
                protocol: PROTOCOL,
                app: me,
                capabilities,
            }),
        };
        let line = encode_line(&hello).map_err(no_answer)?;
        stream.write_all(&line).await.map_err(no_answer)?;
        let mut connection = Self {
            stream,
            decoder: LineDecoder::default(),
            pending: VecDeque::new(),
        };
        loop {
            match connection.next().await? {
                Some(Ok(reply)) => return Ok((welcomed(reply)?, connection)),
                Some(Err(_)) => {}
                None => return Err(no_answer(io::Error::from(io::ErrorKind::UnexpectedEof))),
            }
        }
    }

    /// The next frame from the daemon, a message or why it isn't one; `None` once it hangs up.
    ///
    /// # Errors
    /// When reading from the daemon fails.
    pub async fn next(&mut self) -> Result<Option<Result<Message, FrameError>>, Fault> {
        let mut buf = [0; READ_CHUNK];
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Ok(Some(frame.map(|envelope| envelope.message)));
            }
            let read = self.stream.read(&mut buf).await.map_err(no_answer)?;
            let Some(bytes) = buf.get(..read).filter(|bytes| !bytes.is_empty()) else {
                return Ok(None);
            };
            self.pending.extend(self.decoder.push(bytes));
        }
    }
}

fn not_running(error: io::Error) -> Fault {
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
}

fn no_answer(error: impl Into<Box<dyn Error + Send + Sync>>) -> Fault {
    Fault::new(code!("daemon.no_answer"), "The daemon didn't answer").because(error)
}

fn welcomed(message: Message) -> Result<Welcome, Fault> {
    if let Message::Welcome(welcome) = message {
        return Ok(welcome);
    }
    if let Message::Error(body) = &message
        && body.code == code!("protocol.unsupported")
    {
        return Err(Fault::new(
            code!("daemon.other_protocol"),
            "The daemon speaks another protocol version; restart it",
        ));
    }
    Err(Fault::new(
        code!("daemon.refused"),
        "The daemon refused the handshake",
    ))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::task::JoinHandle;
    use weacon_protocol::{ErrorBody, RunEvent, RunUpdate};
    use weacon_testing::TempDir;

    use super::*;
    use crate::platform::Server;

    fn app(name: &str) -> App {
        App {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        }
    }

    // A daemon that reads the hello, sends `bytes` and hangs up, handing back the hello.
    fn send_after_hello(socket: &Path, bytes: Vec<u8>) -> JoinHandle<Envelope> {
        let server = Server::bind(socket).unwrap();
        tokio::spawn(async move {
            let mut stream = BufReader::new(server.accept().await.unwrap());
            let mut hello = String::new();
            stream.read_line(&mut hello).await.unwrap();
            stream.write_all(&bytes).await.unwrap();
            serde_json::from_str(&hello).unwrap()
        })
    }

    #[tokio::test]
    async fn a_welcome_gives_the_daemons_build_and_capabilities_after_our_hello() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let welcome = Welcome {
            app: app("weacon-core"),
            capabilities: serde_json::from_str(r#"["chat.send"]"#).unwrap(),
        };
        let mut bytes = b"not json\n".to_vec();
        bytes.extend(
            encode_line(&Envelope {
                id: 1,
                message: Message::Welcome(welcome.clone()),
            })
            .unwrap(),
        );
        let daemon = send_after_hello(&socket, bytes);

        assert_eq!(
            handshake(&socket, app("weacon-extension")).await.unwrap(),
            welcome
        );
        assert_eq!(
            daemon.await.unwrap(),
            Envelope {
                id: 1,
                message: Message::Hello(Hello {
                    protocol: PROTOCOL,
                    app: app("weacon-extension"),
                    capabilities: Vec::new(),
                }),
            }
        );
    }

    fn update(text: &str) -> Message {
        Message::Run(RunUpdate {
            run: serde_json::from_str(r#""r-1""#).unwrap(),
            event: RunEvent::TextDelta {
                text: text.to_owned(),
            },
        })
    }

    #[tokio::test]
    async fn what_follows_the_welcome_arrives_in_order_until_the_daemon_hangs_up() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let welcome = Welcome {
            app: app("weacon-core"),
            capabilities: Vec::new(),
        };
        let line = |message| encode_line(&Envelope { id: 0, message }).unwrap();
        let mut bytes = line(Message::Welcome(welcome.clone()));
        bytes.extend(line(update("He")));
        bytes.extend(b"not json\n");
        bytes.extend(line(update("llo")));
        let daemon = send_after_hello(&socket, bytes);
        let offered = vec![Capability::new(code!("run.stream"))];

        let (welcomed, mut connection) =
            Connection::open(&socket, app("weacon-extension"), offered.clone())
                .await
                .unwrap();
        let hello = daemon.await.unwrap();

        assert!(matches!(hello.message, Message::Hello(hello) if hello.capabilities == offered));
        assert_eq!(welcomed, welcome);
        assert_eq!(connection.next().await.unwrap(), Some(Ok(update("He"))));
        assert_eq!(
            connection.next().await.unwrap(),
            Some(Err(FrameError::Invalid))
        );
        assert_eq!(connection.next().await.unwrap(), Some(Ok(update("llo"))));
        assert_eq!(connection.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_daemon_that_hangs_up_gave_no_answer() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        let daemon = send_after_hello(&socket, Vec::new());
        let error = handshake(&socket, app("weacon-extension"))
            .await
            .unwrap_err();
        drop(daemon.await);

        assert_eq!(error.code().as_str(), "daemon.no_answer");
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

    #[tokio::test]
    async fn no_socket_or_a_dead_one_means_no_daemon() {
        let dir = TempDir::new().unwrap();
        let socket = dir.join("weacon.sock");
        drop(Server::bind(&socket).unwrap());

        for path in [socket.as_path(), Path::new("/nonexistent/weacon.sock")] {
            let error = handshake(path, app("weacon-extension")).await.unwrap_err();
            let raw = connect(path).await.err().unwrap();

            assert_eq!(error.code().as_str(), "daemon.not_running");
            assert_eq!(raw.code().as_str(), "daemon.not_running");
        }
    }
}
