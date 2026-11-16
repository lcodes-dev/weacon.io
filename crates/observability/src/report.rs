//! Builds error reports: one OTLP log record for an error, as OTLP/HTTP JSON.

use std::error::Error;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use weacon_protocol::{App, Code};

use crate::errors::describe_error;

const SEVERITY_ERROR: u8 = 17;

/// Where error reports go, and the build they are from.
#[derive(Clone, Debug)]
pub struct Reports {
    url: String,
    build: App,
}

/// One report, ready to POST to `url` as `application/json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub url: String,
    pub body: Vec<u8>,
}

impl Reports {
    /// Reports from `build` to the OTLP endpoint whose base URL is `endpoint`.
    #[must_use]
    pub fn new(endpoint: &str, build: App) -> Self {
        Self {
            url: format!("{}/v1/logs", endpoint.trim().trim_end_matches('/')),
            build,
        }
    }

    /// A report of `error` under `code`: its types, never a message.
    pub fn upload<E: Error + 'static>(&self, error: &E, code: &Code, time: SystemTime) -> Upload {
        let info = describe_error(error);
        let types: Vec<_> = std::iter::successors(Some(&info), |info| info.cause.as_deref())
            .map(|info| info.name.as_deref().unwrap_or("Error"))
            .collect();
        let nanos = time
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let record = json!({
            // OTLP/JSON writes 64-bit integers as decimal strings.
            "timeUnixNano": nanos.to_string(),
            "severityNumber": SEVERITY_ERROR,
            "severityText": "ERROR",
            "eventName": "exception",
            "body": text(code.as_str()),
            "attributes": [
                attribute("exception.type", &types.join(": ")),
                attribute("weacon.code", code.as_str()),
            ],
        });
        let body = json!({ "resourceLogs": [{
            "resource": { "attributes": [
                attribute("service.name", &self.build.name),
                attribute("service.version", &self.build.version),
                attribute("vcs.ref.head.revision", &self.build.commit),
            ] },
            "scopeLogs": [{ "scope": { "name": "weacon" }, "logRecords": [record] }],
        }] });
        Upload {
            url: self.url.clone(),
            body: body.to_string().into_bytes(),
        }
    }
}

fn attribute(key: &str, value: &str) -> Value {
    json!({ "key": key, "value": text(value) })
}

fn text(value: &str) -> Value {
    json!({ "stringValue": value })
}

#[cfg(test)]
mod tests {
    use std::fmt;
    use std::io;
    use std::time::Duration;

    use weacon_protocol::code;

    use super::*;
    use crate::Fault;

    #[derive(Debug)]
    struct Leaky;

    impl fmt::Display for Leaky {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("could not read /home/ada/MARKER-PATH with token MARKER-TOKEN")
        }
    }

    impl Error for Leaky {}

    fn time() -> SystemTime {
        UNIX_EPOCH + Duration::from_hours(489_466)
    }

    fn reports(endpoint: &str) -> Reports {
        let build = App {
            name: "weacon-core".to_owned(),
            version: "0.1.0".to_owned(),
            commit: "1a2b3c4".to_owned(),
        };
        Reports::new(endpoint, build)
    }

    fn sent(upload: &Upload) -> Value {
        serde_json::from_slice(&upload.body).unwrap()
    }

    #[test]
    fn a_report_goes_to_the_endpoints_logs_path() {
        for endpoint in ["http://localhost:4318", " http://localhost:4318/ "] {
            let upload = reports(endpoint).upload(&Leaky, &code!("doctor.failed"), time());
            assert_eq!(upload.url, "http://localhost:4318/v1/logs");
        }
        let under = reports("https://otel.example.com/collector/");
        let upload = under.upload(&Leaky, &code!("doctor.failed"), time());
        assert_eq!(upload.url, "https://otel.example.com/collector/v1/logs");
    }

    #[test]
    fn a_report_is_one_otlp_log_record_with_the_build_the_types_and_the_code_but_no_text() {
        let error = Fault::new(code!("store.full"), "The store is full").because(Leaky);
        let upload = reports("http://localhost:4318").upload(&error, &code!("store.full"), time());

        assert_eq!(
            sent(&upload),
            json!({ "resourceLogs": [{
                "resource": { "attributes": [
                    { "key": "service.name", "value": { "stringValue": "weacon-core" } },
                    { "key": "service.version", "value": { "stringValue": "0.1.0" } },
                    { "key": "vcs.ref.head.revision", "value": { "stringValue": "1a2b3c4" } },
                ] },
                "scopeLogs": [{ "scope": { "name": "weacon" }, "logRecords": [{
                    "timeUnixNano": "1762077600000000000",
                    "severityNumber": 17,
                    "severityText": "ERROR",
                    "eventName": "exception",
                    "body": { "stringValue": "store.full" },
                    "attributes": [
                        { "key": "exception.type", "value": {
                            "stringValue": "weacon_observability::errors::Fault: Error" } },
                        { "key": "weacon.code", "value": { "stringValue": "store.full" } },
                    ],
                }] }],
            }] })
        );
        assert!(!String::from_utf8_lossy(&upload.body).contains("MARKER"));
    }

    #[test]
    fn an_io_error_is_reported_by_its_type() {
        let error = io::Error::new(io::ErrorKind::NotFound, "/home/ada/MARKER-PATH");
        let upload =
            reports("http://localhost:4318").upload(&error, &code!("doctor.failed"), time());
        let body = String::from_utf8(upload.body).unwrap();

        assert!(
            body.contains(r#""stringValue":"std::io::error::Error""#),
            "{body}"
        );
        assert!(!body.contains("MARKER"), "{body}");
    }
}
