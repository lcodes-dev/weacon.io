//! A map of a Rust codebase: its outline, and anchors into it that notice when the code changes.
//!
//! It does no I/O: the caller reads the files and passes their contents in.

mod anchor;
mod fingerprint;
mod index;
mod outline;

use std::fmt;

pub use anchor::{Anchor, Finding, Pin};
pub use fingerprint::Fingerprint;
pub use index::{Crate, Index, Module, SourceFile, index};
pub use outline::{Kind, Symbol};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Grammar,
    Parse,
    Manifest,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Grammar => "the Rust grammar can't be loaded",
            Self::Parse => "a file couldn't be parsed",
            Self::Manifest => "a Cargo.toml isn't valid",
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
        assert_eq!(Error::Manifest.to_string(), "a Cargo.toml isn't valid");
    }
}
