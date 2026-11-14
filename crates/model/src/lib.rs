//! The rules of tasks and docs: their order, their titles, their revisions and who made them.
//!
//! It does no I/O.

mod doc;
mod merge;
mod order;
mod revision;
mod task;
mod title;

pub use doc::{Doc, DocText, DocTextError, ReviseError};
pub use merge::{Hunk, Merge, merge};
pub use order::OrderKey;
pub use revision::{Attribution, Revision};
pub use task::{EditError, Task, TaskEdit};
pub use title::Title;
