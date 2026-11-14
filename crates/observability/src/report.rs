//! Builds error reports: a scrubbed Sentry event for an error, ready to upload.

use std::borrow::Cow;
use std::error::Error;
use std::time::SystemTime;

use sentry_types::protocol::v7::{Envelope, Event, Exception, Map, Uuid, Values};
use sentry_types::{Dsn, Scheme};
use weacon_protocol::{Code, code};

use crate::errors::{ErrorInfo, Fault, describe_error};

mod scrub;

/// Where error reports go, from a Sentry DSN.
#[derive(Clone, Debug)]
pub struct Reports {
    dsn: Dsn,
    client: String,
    release: String,
    home_dir: String,
}

/// One report, ready to POST: the body goes to `url` with `auth` as `X-Sentry-Auth`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub url: String,
    pub auth: String,
    pub body: Vec<u8>,
}

impl Reports {
    /// Reports from `client` running `release`, with `home_dir` written as "~".
    ///
    /// # Errors
    /// When `dsn` isn't a valid DSN.
    pub fn new(dsn: &str, client: &str, release: &str, home_dir: &str) -> Result<Self, Fault> {
        let invalid = || {
            Fault::new(
                code!("report.dsn_invalid"),
                "The DSN isn't a valid https DSN",
            )
        };
        let dsn: Dsn = dsn
            .trim()
            .parse()
            .map_err(|error| invalid().because(error))?;
        if dsn.scheme() != Scheme::Https {
            return Err(invalid());
        }
        Ok(Self {
            dsn,
            client: client.to_owned(),
            release: release.to_owned(),
            home_dir: home_dir.to_owned(),
        })
    }

    /// A scrubbed report of `error`, tagged with `code`, without any error message. The caller
    /// picks the id and the time.
    pub fn upload<E: Error + 'static>(
        &self,
        error: &E,
        code: &Code,
        event_id: u128,
        time: SystemTime,
    ) -> Upload {
        let mut event = Event {
            event_id: Uuid::from_u128(event_id),
            timestamp: time,
            release: Some(Cow::Owned(self.release.clone())),
            exception: Values {
                values: exceptions(&describe_error(error)),
            },
            tags: Map::from([("code".to_owned(), code.as_str().to_owned())]),
            ..Event::default()
        };
        scrub::scrub_event(&mut event, &self.home_dir);
        let mut envelope = Envelope::new();
        envelope.add_item(event);
        let mut body = Vec::new();
        // Writing to a Vec can't fail.
        drop(envelope.to_writer(&mut body));
        Upload {
            url: self.dsn.envelope_api_url().to_string(),
            auth: self.dsn.to_auth(Some(&self.client)).to_string(),
            body,
        }
    }
}

// Sentry lists the root cause first and the error itself last.
fn exceptions(info: &ErrorInfo) -> Vec<Exception> {
    let mut chain: Vec<Exception> = std::iter::successors(Some(info), |info| info.cause())
        .map(|info| Exception {
            ty: info.type_name().unwrap_or("Error").to_owned(),
            ..Exception::default()
        })
        .collect();
    chain.reverse();
    chain
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::io;
    use std::time::Duration;

    use sentry_types::protocol::v7::{EnvelopeItem, Level};

    use super::*;

    const DSN: &str = "https://public@sentry.example.com/42";

    #[derive(Debug)]
    struct Leaky;

    impl fmt::Display for Leaky {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("could not read /home/ada/MARKER-PATH with token MARKER-TOKEN")
        }
    }

    impl Error for Leaky {}

    fn time() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_hours(489_466)
    }

    fn reports() -> Reports {
        Reports::new(
            DSN,
            "weacon/0.1.0",
            "weacon-core@0.1.0+1a2b3c4",
            "/home/ada",
        )
        .unwrap()
    }

    fn sent_event(upload: &Upload) -> Event<'static> {
        let envelope = Envelope::from_slice(&upload.body).unwrap();
        let Some(EnvelopeItem::Event(event)) = envelope.items().next() else {
            panic!("the envelope holds no event");
        };
        event.clone().into_owned()
    }

    #[test]
    fn a_report_goes_to_the_dsn_with_its_auth() {
        let reports = reports();
        let upload = reports.upload(&Leaky, &code!("doctor.failed"), 7, time());

        assert_eq!(upload.url, "https://sentry.example.com/api/42/envelope/");
        assert!(upload.auth.contains("sentry_key=public"), "{}", upload.auth);
        assert!(
            upload.auth.contains("sentry_client=weacon/0.1.0"),
            "{}",
            upload.auth
        );
    }

    #[test]
    fn a_report_carries_ids_types_and_the_code_but_no_text() {
        let reports = reports();
        let error = Fault::new(code!("store.full"), "The store is full").because(Leaky);
        let upload = reports.upload(&error, &code!("store.full"), 7, time());
        let event = sent_event(&upload);
        let types: Vec<_> = event
            .exception
            .values
            .iter()
            .map(|ex| ex.ty.as_str())
            .collect();

        assert_eq!(event.event_id, Uuid::from_u128(7));
        assert_eq!(event.timestamp, time());
        assert_eq!(event.level, Level::Error);
        assert_eq!(event.release.as_deref(), Some("weacon-core@0.1.0+1a2b3c4"));
        assert_eq!(
            event.tags.get("code").map(String::as_str),
            Some("store.full")
        );
        assert_eq!(types, ["Error", "weacon_observability::errors::Fault"]);
        assert!(!String::from_utf8_lossy(&upload.body).contains("MARKER"));
    }

    #[test]
    fn an_io_error_is_reported_by_its_type() {
        let reports = reports();
        let error = io::Error::new(io::ErrorKind::NotFound, "/home/ada/MARKER-PATH");
        let event = sent_event(&reports.upload(&error, &code!("doctor.failed"), 7, time()));

        assert_eq!(event.exception.values.len(), 1);
        assert_eq!(event.exception.values[0].ty, "std::io::error::Error");
        assert_eq!(event.exception.values[0].value, None);
    }

    #[test]
    fn an_invalid_dsn_is_refused() {
        for dsn in ["not a dsn", "http://public@sentry.example.com/42"] {
            let error = Reports::new(dsn, "weacon/0.1.0", "", "").unwrap_err();

            assert_eq!(error.code().as_str(), "report.dsn_invalid");
        }
    }
}
