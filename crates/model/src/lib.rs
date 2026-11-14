//! The rules of tasks, docs and chats: their order, their titles, their revisions, who made
//! them, and what a chat's log says now.
//!
//! It does no I/O.

mod chat;
mod doc;
mod merge;
mod order;
mod revision;
mod task;
mod title;

pub use chat::{ChatRuns, Waiting};
pub use doc::{Doc, DocText, DocTextError, ReviseError};
pub use merge::{Hunk, Merge, merge};
pub use order::OrderKey;
pub use revision::{Attribution, Revision};
pub use task::{EditError, Task, TaskEdit};
pub use title::Title;
