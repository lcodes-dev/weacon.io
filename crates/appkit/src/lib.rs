//! What every Weacon app wires in: its logger, error reports, log files, signals, runtime and
//! how it ends.

mod app;
mod deliver;
mod exit;
mod id;
mod logs;
mod platform;
mod runtime;
#[cfg(test)]
mod test_support;

pub use app::{build, panic_hook, reporter, reports, root_logger};
pub use deliver::http;
pub use exit::{Ending, finish};
pub use id::random_id;
pub use logs::RotatingFile;
pub use platform::{Signals, owner_only, private_append, sync_dir, write_whole};
pub use runtime::runtime;
