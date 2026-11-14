//! The rules of tasks: their order, their titles, their revisions and who made them.
//!
//! It does no I/O.

mod order;
mod revision;
mod task;
mod title;

pub use order::OrderKey;
pub use revision::{Attribution, Revision};
pub use task::{EditError, Task, TaskEdit};
pub use title::Title;
