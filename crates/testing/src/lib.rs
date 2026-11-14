//! Test helpers for Weacon's crates.

mod fake_chat;
pub mod fake_core;
mod fake_diff;
pub mod fake_server;
mod logs;
pub mod platform;
pub mod recording;
mod replay;
mod temp;

pub use logs::{Records, Uploads, messages, recording_deliver, recording_logger, recording_sink};
pub use replay::{Replay, replay};
pub use temp::TempDir;

/// Why a fake failed: the first way its peer strayed from the script, or a failed connection.
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
