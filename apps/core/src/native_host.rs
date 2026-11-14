use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use weacon_observability::{Fault, Logger};
use weacon_protocol::{Envelope, FrameError, LineDecoder, MAX_FRAME, code};

use crate::app::{App, runtime};
use crate::daemon::{READ_CHUNK, socket_path};
use crate::platform::{self, CHROME_HOST, Env};

/// How Chrome names the extension; its id comes from the public key in its manifest.
const ORIGIN: &str = "chrome-extension://phnfnmnpncfhkhlkmglnlhlkaaociaij/";

// What tells Chrome which program to start for the extension.
#[derive(Serialize)]
struct HostManifest<'a> {
    name: &'static str,
    description: &'static str,
    path: &'a str,
    r#type: &'static str,
    allowed_origins: [&'static str; 1],
}

/// Registers this program as Chrome's native host, so the extension can reach the daemon.
pub(crate) fn connect_chrome(app: &App, env: Env<'_>, out: &mut impl Write) -> Result<(), Fault> {
    register(app, env, std::env::current_exe(), out)
}

fn register(
    app: &App,
    env: Env<'_>,
    exe: io::Result<PathBuf>,
    out: &mut impl Write,
) -> Result<(), Fault> {
    let state_dir = app.state_dir()?;
    let failed = |error: io::Error| {
        Fault::new(
            code!("chrome.connect_failed"),
            "Chrome's native host couldn't be registered",
        )
        .because(error)
    };
    let manifest = exe.and_then(|exe| manifest(&exe)).map_err(failed)?;
    let written = platform::register_chrome_host(env, state_dir, &manifest).map_err(failed)?;
    writeln!(
        out,
        "Chrome can reach this core once the extension is installed."
    )
    .map_err(failed)?;
    written
        .iter()
        .try_for_each(|path| writeln!(out, "Registered: {}", path.display()))
        .map_err(failed)
}

fn manifest(exe: &Path) -> io::Result<Vec<u8>> {
    let path = exe
        .to_str()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidFilename))?;
    let manifest = HostManifest {
        name: CHROME_HOST,
        description: "Weacon's core",
        path,
        r#type: "stdio",
        allowed_origins: [ORIGIN],
    };
    serde_json::to_vec_pretty(&manifest).map_err(io::Error::other)
}

/// Relays Chrome's native messages between the extension and the daemon until either hangs up.
pub(crate) fn run(app: &App, origin: &str) -> Result<(), Fault> {
    if origin != ORIGIN {
        return Err(Fault::new(
            code!("native_host.origin_refused"),
            "Only Weacon's extension may start the native host",
        ));
    }
    let state_dir = app.state_dir()?;
    let socket = socket_path(state_dir);
    let runtime = runtime()?;
    let relayed = runtime.block_on(async {
        let daemon = weacon_client::connect(&socket).await?;
        let relay = Box::pin(relay(
            tokio::io::stdin(),
            tokio::io::stdout(),
            daemon,
            &app.log,
        ));
        crate::reports::following(state_dir, &app.reporter, &app.log, relay)
            .await
            .map_err(|error| {
                Fault::new(code!("native_host.failed"), "The native host stopped").because(error)
            })
    });
    // Tokio's stdin read can't be cancelled, so waiting for it would outlive the daemon.
    runtime.shutdown_background();
    relayed
}

// Either direction ending ends both, so Chrome and the daemon each see the other hang up.
async fn relay(
    chrome_in: impl AsyncRead + Unpin,
    chrome_out: impl AsyncWrite + Unpin,
    daemon: impl AsyncRead + AsyncWrite + Unpin,
    log: &Logger,
) -> io::Result<()> {
    let (daemon_in, daemon_out) = tokio::io::split(daemon);
    tokio::select! {
        sent = to_daemon(chrome_in, daemon_out, log) => sent,
        sent = to_chrome(daemon_in, chrome_out, log) => sent,
    }
}

// Chrome frames a message as its length, a native-endian u32, then that many bytes of JSON. A
// frame that isn't a message ends the relay, so the extension sees the port close instead of
// waiting for an answer that won't come.
async fn to_daemon(
    mut chrome: impl AsyncRead + Unpin,
    mut daemon: impl AsyncWrite + Unpin,
    log: &Logger,
) -> io::Result<()> {
    loop {
        let mut prefix = [0; 4];
        match chrome.read_exact(&mut prefix).await {
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            read => read?,
        };
        let length = u64::from(u32::from_ne_bytes(prefix));
        // A frame of the limit leaves no room for the newline that ends its line.
        if usize::try_from(length).is_ok_and(|length| length >= MAX_FRAME) {
            refused(log, "frame.too_large");
            return Ok(());
        }
        let mut json = Vec::new();
        (&mut chrome).take(length).read_to_end(&mut json).await?;
        match as_line(&json) {
            Ok(line) => daemon.write_all(&line).await?,
            Err(error) => {
                refused(log, error.code().as_str());
                return Ok(());
            }
        }
    }
}

// A frame that parses as a message goes on as it came, compacted onto one line, so the daemon
// sees the fields and message types of an extension newer than this build and answers them.
fn as_line(json: &[u8]) -> Result<Vec<u8>, FrameError> {
    Envelope::from_json(json)?;
    let value: serde_json::Value = serde_json::from_slice(json).map_err(|_| FrameError::Invalid)?;
    let mut line = serde_json::to_vec(&value).map_err(|_| FrameError::Invalid)?;
    line.push(b'\n');
    Ok(line)
}

async fn to_chrome(
    mut daemon: impl AsyncRead + Unpin,
    mut chrome: impl AsyncWrite + Unpin,
    log: &Logger,
) -> io::Result<()> {
    let mut decoder = LineDecoder::default();
    let mut buf = [0; READ_CHUNK];
    loop {
        let read = daemon.read(&mut buf).await?;
        if read == 0 {
            return Ok(());
        }
        for received in decoder.push(buf.get(..read).unwrap_or_default()) {
            match received.and_then(|envelope| envelope.to_json()) {
                Ok(json) => {
                    let length = u32::try_from(json.len()).map_err(io::Error::other)?;
                    chrome.write_all(&length.to_ne_bytes()).await?;
                    chrome.write_all(&json).await?;
                }
                Err(error) => refused(log, error.code().as_str()),
            }
        }
        chrome.flush().await?;
    }
}

fn refused(log: &Logger, code: &str) {
    log.warn("native message refused", &[("code", code.into())]);
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncBufReadExt, BufReader, DuplexStream, duplex};
    use weacon_protocol::{ErrorBody, Message, encode_line};
    use weacon_testing::TempDir;

    use super::*;
    use crate::test_support::{self, messages, recording_logger};

    fn app(state_dir: Option<&Path>) -> App {
        test_support::app(state_dir, None)
    }

    fn code(result: Result<(), Fault>) -> String {
        result.unwrap_err().code().as_str().to_owned()
    }

    #[cfg(unix)]
    #[test]
    fn connecting_chrome_registers_this_program_and_says_where() {
        use std::fs;

        use crate::test_support::env;

        let dir = TempDir::new().unwrap();
        let home = dir.join("home");
        let vars = env(&[("HOME", home.to_str().unwrap())]);
        let exe = || Ok(PathBuf::from("/opt/weacon/weacon"));
        let mut out = Vec::new();

        register(&app(Some(&dir)), &vars, exe(), &mut out).unwrap();

        let out = String::from_utf8(out).unwrap();
        let registered: Vec<_> = out
            .lines()
            .filter_map(|line| line.strip_prefix("Registered: "))
            .map(PathBuf::from)
            .collect();
        assert!(out.starts_with("Chrome can reach this core"), "{out}");
        assert_eq!(registered.len(), 2, "{out}");
        for path in registered {
            assert!(path.starts_with(&home), "{out}");
            assert!(
                fs::read_to_string(&path)
                    .unwrap()
                    .contains("/opt/weacon/weacon")
            );
        }
        let unknown = Err(io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(
            code(register(&app(Some(&dir)), &vars, unknown, &mut Vec::new())),
            "chrome.connect_failed"
        );
        assert_eq!(
            code(register(&app(None), &vars, exe(), &mut Vec::new())),
            "state.unavailable"
        );
    }

    #[test]
    fn the_host_runs_only_for_the_extension_and_needs_the_daemon() {
        let dir = TempDir::new().unwrap();

        assert_eq!(
            code(run(&app(Some(&dir)), "chrome-extension://other/")),
            "native_host.origin_refused"
        );
        assert_eq!(code(run(&app(None), ORIGIN)), "state.unavailable");
        assert_eq!(code(run(&app(Some(&dir)), ORIGIN)), "daemon.not_running");
    }

    fn error(id: u32) -> Envelope {
        Envelope {
            id,
            message: Message::Error(ErrorBody {
                code: code!("store.full"),
                retryable: false,
            }),
        }
    }

    fn framed(json: &[u8]) -> Vec<u8> {
        let mut frame = u32::try_from(json.len()).unwrap().to_ne_bytes().to_vec();
        frame.extend(json);
        frame
    }

    async fn read_frame(chrome: &mut DuplexStream) -> Envelope {
        let mut prefix = [0; 4];
        chrome.read_exact(&mut prefix).await.unwrap();
        let mut json = vec![0; usize::try_from(u32::from_ne_bytes(prefix)).unwrap()];
        chrome.read_exact(&mut json).await.unwrap();
        Envelope::from_json(&json).unwrap()
    }

    #[tokio::test]
    async fn frames_cross_both_ways_and_bad_lines_from_the_daemon_are_dropped() {
        let (log, records) = recording_logger();
        let (mut chrome, host_chrome) = duplex(64 * 1024);
        let (daemon, host_daemon) = duplex(64 * 1024);
        let (chrome_in, chrome_out) = tokio::io::split(host_chrome);
        let relayed =
            tokio::spawn(async move { relay(chrome_in, chrome_out, host_daemon, &log).await });
        chrome
            .write_all(&framed(&error(1).to_json().unwrap()))
            .await
            .unwrap();
        let mut daemon = BufReader::new(daemon);
        let mut line = String::new();
        // A relay that never sends the line would leave this waiting; fail instead.
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            daemon.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        daemon.write_all(b"junk\n").await.unwrap();
        daemon
            .write_all(&encode_line(&error(2)).unwrap())
            .await
            .unwrap();

        assert_eq!(Envelope::from_json(line.trim().as_bytes()), Ok(error(1)));
        assert_eq!(read_frame(&mut chrome).await, error(2));
        drop(chrome);
        relayed.await.unwrap().unwrap();
        assert_eq!(messages(&records), ["native message refused"]);
    }

    #[tokio::test]
    async fn a_newer_extensions_messages_and_fields_reach_the_daemon() {
        let (log, records) = recording_logger();
        let (mut chrome, host_chrome) = duplex(64 * 1024);
        let (daemon, host_daemon) = duplex(64 * 1024);
        let (chrome_in, chrome_out) = tokio::io::split(host_chrome);
        let newer = [
            r#"{ "id": 5, "type": "future.thing", "text": "a\nb" }"#,
            r#"{"id":6,"type":"error","code":"store.full","retryable":false,"hint":2}"#,
        ];
        for json in newer {
            chrome.write_all(&framed(json.as_bytes())).await.unwrap();
        }
        drop(chrome);

        relay(chrome_in, chrome_out, host_daemon, &log)
            .await
            .unwrap();

        let mut lines = BufReader::new(daemon).lines();
        for json in newer {
            let line = lines.next_line().await.unwrap().unwrap();
            let sent: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                sent
            );
        }
        assert!(messages(&records).is_empty());
    }

    #[tokio::test]
    async fn a_frame_from_chrome_that_isnt_a_message_closes_the_port() {
        // Only the length of the large one is sent: it is refused before its body is read.
        let too_large = u32::try_from(MAX_FRAME).unwrap().to_ne_bytes().to_vec();
        for (sent, code) in [
            (framed(b"not json"), "frame.invalid"),
            (too_large, "frame.too_large"),
        ] {
            let (log, records) = recording_logger();
            let (mut chrome, host_chrome) = duplex(64 * 1024);
            let (mut daemon, host_daemon) = duplex(64 * 1024);
            let (chrome_in, chrome_out) = tokio::io::split(host_chrome);
            chrome.write_all(&sent).await.unwrap();
            drop(chrome);

            relay(chrome_in, chrome_out, host_daemon, &log)
                .await
                .unwrap();

            let logged = serde_json::to_string(&*records.lock().unwrap()).unwrap();
            assert_eq!(messages(&records), ["native message refused"]);
            assert!(logged.contains(code), "{logged}");
            assert_eq!(daemon.read(&mut [0; 16]).await.unwrap(), 0);
        }
    }

    // A reader that fails as a broken pipe would.
    struct Broken;

    impl AsyncRead for Broken {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
        }
    }

    #[tokio::test]
    async fn a_failed_read_from_chrome_is_an_error_but_its_end_is_not() {
        let (log, _) = recording_logger();
        let (_daemon, host_daemon) = duplex(64);

        let error = to_daemon(Broken, host_daemon, &log).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn the_daemon_hanging_up_ends_the_relay() {
        let (log, _) = recording_logger();
        let (_chrome, host_chrome) = duplex(64);
        let (daemon, host_daemon) = duplex(64);
        let (chrome_in, chrome_out) = tokio::io::split(host_chrome);
        drop(daemon);
        let relayed = relay(chrome_in, chrome_out, host_daemon, &log);

        // A relay that spins on the closed socket would never end; fail instead of hanging.
        tokio::time::timeout(std::time::Duration::from_secs(10), relayed)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn the_manifest_lets_only_the_extension_start_this_program() {
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest(Path::new("/opt/weacon/weacon")).unwrap()).unwrap();

        assert_eq!(
            manifest,
            serde_json::json!({
                "name": "io.weacon.core",
                "description": "Weacon's core",
                "path": "/opt/weacon/weacon",
                "type": "stdio",
                "allowed_origins": ["chrome-extension://phnfnmnpncfhkhlkmglnlhlkaaociaij/"],
            })
        );
    }
}
