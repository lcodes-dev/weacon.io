//! Logging with redaction, error descriptions and the error-report scrubber, for every app.

mod errors;
mod logger;
mod redact;
mod scrub;

pub use errors::{AsError, Fault};
pub use logger::{Clock, Level, Logger, LoggerOptions, Record, Sink, json_lines};
pub use redact::Field;
pub use scrub::scrub_event;
