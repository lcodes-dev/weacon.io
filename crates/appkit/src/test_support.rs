use std::path::PathBuf;
use std::{env, fs, process};

/// An empty folder of this test's own.
pub(crate) fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("weacon-appkit-{name}-{}", process::id()));
    drop(fs::remove_dir_all(&dir));
    fs::create_dir_all(&dir).unwrap();
    dir
}
