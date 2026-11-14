//! Drives the person's own coding agents and turns their output into run events.

mod access;
mod claude;
mod codex;
pub mod guard;
mod launch;
mod lines;
mod platform;
mod run;
pub mod sessions;
mod spawn_policy;
#[cfg(test)]
mod test_support;
mod transcript;
mod wire;

pub use access::resolve;
pub use launch::{Launch, SignIn};
pub use run::{AgentRun, Run};
pub use spawn_policy::SpawnPolicy;
pub use transcript::{Said, Transcript};
