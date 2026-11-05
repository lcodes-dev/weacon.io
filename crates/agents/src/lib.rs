//! Drives the person's own coding agents and turns their output into run events.

mod claude;
mod codex;
mod lines;
mod run;
mod spawn;
#[cfg(test)]
mod test_support;

pub use claude::Claude;
pub use codex::Codex;
pub use run::{AgentRun, Run};
pub use spawn::SpawnPolicy;
