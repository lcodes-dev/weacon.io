//! The rules of tasks: their order, their titles, their revisions and who changed them.
//!
//! It does no I/O.

mod change;
mod order;
mod task;
mod title;

pub use change::{Change, Revision};
pub use order::OrderKey;
pub use task::{EditError, Task, TaskEdit};
pub use title::Title;
