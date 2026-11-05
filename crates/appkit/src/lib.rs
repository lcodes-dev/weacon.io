//! What every Weacon app wires in: its logger, error reports, log files and signals.

mod app;
mod logs;
mod platform;
mod report;
#[cfg(test)]
mod test_support;

pub use app::{build, panic_hook, reporter, reports, root_logger, version_text};
pub use logs::{KEEP, MAX_BYTES, RotatingFile};
pub use platform::{Signals, private_append};
pub use report::{http, random_id};
