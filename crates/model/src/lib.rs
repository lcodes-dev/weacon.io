//! The rules of tasks: their order, their versions and who changed them.
//!
//! It does no I/O.

mod merge;
mod order;
mod task;

pub use merge::{Hunk, Merge, merge};
pub use order::OrderKey;
pub use task::{Change, EditError, Task, TaskEdit, TaskTitle, TaskVersion};
