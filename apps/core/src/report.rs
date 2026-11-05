use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, SystemTime};

use weacon_observability::{AsError, Fault, Field, Logger, Reports, Upload};
use weacon_protocol::{Code, code};

const TIMEOUT: Duration = Duration::from_secs(5);

/// Delivers one report; the real one POSTs it, tests record it.
pub(crate) type Deliver = Box<dyn Fn(&Upload) -> Result<(), Fault> + Send + Sync>;

/// Sends error reports when a DSN is set, and does nothing otherwise.
pub(crate) struct Reporter {
    reports: Option<Reports>,
    deliver: Deliver,
}

impl Reporter {
    pub(crate) fn new(reports: Option<Reports>, deliver: Deliver) -> Self {
        Self { reports, deliver }
    }

    pub(crate) const fn is_on(&self) -> bool {
        self.reports.is_some()
    }

    /// Reports `error` under `code`; a report that can't be sent is logged and dropped.
    pub(crate) fn capture_error<E: AsError + ?Sized>(&self, error: &E, code: &Code, log: &Logger) {
        let Some(reports) = &self.reports else {
            return;
        };
        let upload = reports.upload(error, code, random_id(), SystemTime::now());
        if let Err(error) = (self.deliver)(&upload) {
            log.warn("report failed", &[("err", Field::error(&error))]);
        }
    }
}

// Each `RandomState` gets new keys; the first on each thread come from the operating system.
fn random_id() -> u128 {
    let half = || RandomState::new().build_hasher().finish();
    u128::from(half()) << 64 | u128::from(half())
}

/// Sends reports over HTTP.
pub(crate) fn http() -> Deliver {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    Box::new(move |upload| {
        agent
            .post(&upload.url)
            .header("X-Sentry-Auth", &upload.auth)
            .header("Content-Type", "application/x-sentry-envelope")
            .send(upload.body.as_slice())
            .map(drop)
            .map_err(|error| {
                Fault::new(
                    code!("report.send_failed"),
                    "The error report couldn't be sent",
                )
                .because(error)
            })
    })
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::thread;

    use weacon_observability::Level;

    use super::*;
    use crate::test_support::{VERSION, messages, recording_logger};

    type Request = (Vec<String>, Vec<u8>);

    fn recorder(fails: bool) -> (Deliver, Arc<Mutex<Vec<Upload>>>) {
        let uploads = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&uploads);
        let deliver: Deliver = Box::new(move |upload| {
            seen.lock().unwrap().push(upload.clone());
            if fails {
                return Err(Fault::new(code!("report.send_failed"), "failed"));
            }
            Ok(())
        });
        (deliver, uploads)
    }

    fn reports() -> Option<Reports> {
        let release = VERSION.release();
        Reports::new(
            "https://public@sentry.example.com/42",
            "weacon/0.1.0",
            &release,
            "/home/ada",
        )
        .ok()
    }

    fn fault() -> Fault {
        Fault::new(code!("store.full"), "The store is full")
    }

    #[test]
    fn without_a_dsn_nothing_is_sent() {
        let (deliver, uploads) = recorder(false);
        let reporter = Reporter::new(None, deliver);
        reporter.capture_error(&fault(), &code!("store.full"), &recording_logger().0);

        assert!(!reporter.is_on());
        assert!(uploads.lock().unwrap().is_empty());
    }

    #[test]
    fn with_a_dsn_each_error_is_sent_once_with_its_own_id() {
        let (deliver, uploads) = recorder(false);
        let reporter = Reporter::new(reports(), deliver);
        let (log, records) = recording_logger();
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        reporter.capture_error(&fault(), &code!("store.full"), &log);
        let uploads = uploads.lock().unwrap();

        assert!(reporter.is_on());
        assert_eq!(uploads.len(), 2);
        assert_ne!(uploads[0].body, uploads[1].body);
        assert!(String::from_utf8_lossy(&uploads[0].body).contains("weacon-core@0.1.0+1a2b3c4"));
        assert!(records.lock().unwrap().is_empty());
    }

    #[test]
    fn a_report_that_cant_be_sent_is_logged() {
        let (deliver, _) = recorder(true);
        let reporter = Reporter::new(reports(), deliver);
        let (log, records) = recording_logger();
        reporter.capture_error(&fault(), &code!("store.full"), &log);

        assert_eq!(messages(&records), ["report failed"]);
        assert_eq!(records.lock().unwrap()[0].level, Level::Warn);
    }

    #[test]
    fn ids_are_random_in_both_halves() {
        let id = random_id();
        let (high, low) = (id >> 64, id & u128::from(u64::MAX));

        assert_ne!(id, random_id());
        assert_ne!(high, 0);
        assert_ne!(high, low);
    }

    // Answers one request with `status`, and hands back its head lines and body.
    fn serve_once(status: &'static str) -> (u16, thread::JoinHandle<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let head: Vec<String> = reader
                .by_ref()
                .lines()
                .map(Result::unwrap)
                .take_while(|line| !line.is_empty())
                .collect();
            let length = head
                .iter()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length: ")?
                        .parse()
                        .ok()
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let reply = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n");
            reader.get_mut().write_all(reply.as_bytes()).unwrap();
            (head, body)
        });
        (port, server)
    }

    fn upload(port: u16) -> Upload {
        Upload {
            url: format!("http://127.0.0.1:{port}/api/42/envelope/"),
            auth: "Sentry sentry_key=public".to_owned(),
            body: b"envelope".to_vec(),
        }
    }

    #[test]
    fn http_posts_the_envelope_with_its_auth() {
        let (port, server) = serve_once("200 OK");
        http()(&upload(port)).unwrap();
        let (head, body) = server.join().unwrap();
        let has = |header: &str| head.iter().any(|line| line.eq_ignore_ascii_case(header));

        assert_eq!(head[0], "POST /api/42/envelope/ HTTP/1.1");
        assert!(has("x-sentry-auth: Sentry sentry_key=public"), "{head:?}");
        assert!(
            has("content-type: application/x-sentry-envelope"),
            "{head:?}"
        );
        assert_eq!(body, b"envelope");
    }

    #[test]
    fn http_reports_a_refused_report() {
        let (port, server) = serve_once("500 Internal Server Error");
        let error = http()(&upload(port)).unwrap_err();
        drop(server.join().unwrap());

        assert_eq!(error.code().as_str(), "report.send_failed");
    }
}
