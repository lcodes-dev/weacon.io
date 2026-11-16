//! Drives the person's own coding agents and turns their output into run events.

mod access;
mod claude;
mod codex;
mod launch;
mod lines;
pub mod outside;
mod platform;
mod run;
pub mod sessions;
mod spawn;
mod terminal;
#[cfg(test)]
mod test_support;
mod transcript;
mod vendor;
mod wire;

pub use access::resolve;
pub use claude::TOKEN_VARIABLE;
pub use launch::{Launch, McpServer, SignIn};
pub use run::{AgentRun, Run};
pub use spawn::SpawnPolicy;
pub use terminal::{Event, Events, Size, Terminal};
pub use transcript::{Step, Transcript};
pub use wire::takes_images;

pub use platform::link_count;
