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

pub use app::{App, build, reporter, reports, root_logger};
pub use deliver::deliver_over_https;
pub use exit::Ending;
pub use id::random_id;
pub use logs::RotatingFile;
pub use platform::{
    Signals, create_private, is_private, private_append, sync_dir, write_private, write_whole,
};
pub use runtime::runtime;
