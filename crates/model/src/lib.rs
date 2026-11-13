//! The rules of tasks and docs: their order, their titles, their revisions and who changed
//! them.
//!
//! It does no I/O.

mod change;
mod doc;
mod order;
mod task;
mod title;

pub use change::{Change, Revision};
pub use doc::{Doc, DocText, DocTextError, ReviseError};
pub use order::OrderKey;
pub use task::{EditError, Task, TaskEdit};
pub use title::Title;
