//! Logging with redaction, error descriptions and error reports, for every app.

mod errors;
mod logger;
mod redact;
mod report;
mod reporter;
mod scrub;

pub use errors::Fault;
pub use logger::{Clock, Logger, LoggerOptions, Record, Sink, json_lines, tee};
pub use redact::Field;
pub use report::{Reports, Upload};
pub use reporter::{Deliver, Ids, Reporter};
pub use weacon_protocol::Level;
