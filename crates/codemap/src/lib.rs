//! A map of a Rust codebase: its outline, its areas, and anchors into it that notice when the
//! code changes.
//!
//! It does no I/O: the caller reads the files and passes their contents in.

mod anchor;
mod answers;
mod area;
mod contracts;
mod edges;
mod facts;
mod fingerprint;
mod flows;
mod graph;
mod index;
mod louvain;
mod paths;
mod rank;
mod refs;
mod search;
mod symbols;
mod syntax;
#[cfg(test)]
mod test_support;

use std::fmt;

pub use anchor::{Anchor, Finding, Pin};
pub use answers::{Churn, fit};
pub(crate) use area::Area;
pub use contracts::ContractKind;
pub(crate) use edges::{Edge, EdgeKind};
pub use facts::{Facts, Reader};
pub use fingerprint::Fingerprint;
pub(crate) use flows::Break;
pub(crate) use index::{Crate, Module};
pub use index::{Index, SourceFile, assemble, index};
pub use search::Entry;
pub(crate) use symbols::{Kind, Symbol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Grammar,
    Parse,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Grammar => "the Rust grammar can't be loaded",
            Self::Parse => "a file couldn't be parsed",
        })
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_errors_without_content() {
        assert_eq!(
            Error::Grammar.to_string(),
            "the Rust grammar can't be loaded"
        );
        assert_eq!(Error::Parse.to_string(), "a file couldn't be parsed");
    }
}
