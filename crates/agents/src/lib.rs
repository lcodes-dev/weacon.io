//! Drives the person's own coding agents and turns their output into run events.

mod access;
mod claude;
mod codex;
mod launch;
mod lines;
mod platform;
mod run;
mod spawn;
#[cfg(test)]
mod test_support;
mod wire;

pub use launch::{Launch, SignIn};
pub use run::{AgentRun, Run};
pub use spawn::SpawnPolicy;
