//! Drives the person's own coding agents and turns their output into run events.

mod claude;

pub use claude::{Claude, ClaudeRun, Run};
