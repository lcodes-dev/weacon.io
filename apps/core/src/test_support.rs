use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::SystemTime;

use weacon_observability::Reporter;
use weacon_protocol::{Message, Pick, RequestId};
use weacon_version::Version;

pub(crate) mod tls;

pub(crate) use weacon_testing::fake_core::client_hello;
pub(crate) use weacon_testing::{Records, messages, recording_logger, recording_sink};

use crate::app::App;

pub(crate) const VERSION: Version = Version {
    name: "weacon-core",
    version: "0.1.0",
    commit: "1a2b3c4",
};

/// A fake environment holding only `vars`.
pub(crate) fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let vars: HashMap<String, OsString> = vars
        .iter()
        .map(|(key, value)| ((*key).to_owned(), OsString::from(value)))
        .collect();
    move |key| vars.get(key).cloned()
}

/// An app whose state is in `state_dir`, logging nowhere and reporting nothing.
pub(crate) fn app(state_dir: Option<&Path>, log_file: Option<&Path>) -> App {
    App {
        base: weacon_appkit::App {
            version: VERSION,
            log: recording_logger().0,
            reporter: Reporter::new(
                None,
                true,
                Box::new(|_| Ok(())),
                Box::new(|| 0),
                Box::new(SystemTime::now),
            )
            .into(),
        },
        state_dir: state_dir.map(Path::to_path_buf),
        work_root: None,
        log_file: log_file.map(Path::to_path_buf),
        spawn: weacon_agents::SpawnPolicy::default(),
        path: None,
    }
}

/// A pick of a button on a local page, with `html` as its HTML.
pub(crate) fn pick(request: &str, html: &str) -> Pick {
    Pick {
        request: RequestId::try_from(request.to_owned()).unwrap(),
        url: "http://localhost:3000/".to_owned(),
        title: "Home".to_owned(),
        selector: "main > button".to_owned(),
        html: html.to_owned(),
        screenshot_png: Some("iVBORw0KGgo=".to_owned()),
    }
}

/// A reader and writer, sync or async, whose every read and write fails.
pub(crate) struct Broken;

impl Read for Broken {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
}

impl Write for Broken {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl tokio::io::AsyncRead for Broken {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()))
    }
}

/// The code of an error message, if it is one.
pub(crate) fn error_code(message: &Message) -> Option<&str> {
    let Message::Error(body) = message else {
        return None;
    };
    Some(body.code.as_str())
}
