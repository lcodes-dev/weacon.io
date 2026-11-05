//! What every Weacon app wires in: rotating log files and error report delivery.

mod logs;
mod platform;
mod report;

pub use logs::{KEEP, MAX_BYTES, RotatingFile};
pub use platform::private_append;
pub use report::{http, random_id};

/// An empty folder of this test's own.
#[cfg(test)]
fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("weacon-appkit-{name}-{}", std::process::id()));
    drop(std::fs::remove_dir_all(&dir));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
