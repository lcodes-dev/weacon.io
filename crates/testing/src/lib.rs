//! Test helpers for Weacon's crates.

pub mod fake_core;
mod fake_diff;
pub mod fake_server;
mod logs;
pub mod platform;
pub mod recording;
mod replay;
mod temp;

pub use logs::{Records, messages, recording_logger, recording_sink};
pub use replay::{Replay, replay};
pub use temp::TempDir;
