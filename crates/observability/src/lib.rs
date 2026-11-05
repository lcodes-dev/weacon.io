//! Logging with redaction, error descriptions and error reports, for every app.

mod errors;
mod logger;
mod redact;
mod report;
mod scrub;

pub use errors::{AsError, Fault};
pub use logger::{Clock, Level, Logger, LoggerOptions, Record, Sink, json_lines};
pub use redact::Field;
pub use report::{Reports, Upload};
