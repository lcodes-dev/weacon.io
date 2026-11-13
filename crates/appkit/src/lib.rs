//! What every Weacon app wires in: its logger, error reports, log files, signals, runtime and
//! how it ends.

mod app;
mod exit;
mod logs;
mod platform;
mod report;
#[cfg(test)]
mod test_support;

pub use app::{build, panic_hook, reporter, reports, root_logger, version_text};
pub use exit::{Outcome, finish, runtime};
pub use logs::{KEEP, MAX_BYTES, RotatingFile};
pub use platform::{Signals, owner_only, private_append, sync_dir, write_whole};
pub use report::{http, random_id};
