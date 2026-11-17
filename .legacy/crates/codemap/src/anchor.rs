use std::fmt;
use std::str::FromStr;

use crate::{Area, Fingerprint, Index};

/// A place in the code that docs and tasks point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// `sym:weacon_protocol::frame::encode_line`: one item.
    Symbol(String),
    /// `flow:weacon_core::cli::run->weacon_core::daemon::serve`: items a flow passes through, in order.
    Flow(Vec<String>),
    /// `area:crates/protocol/src/lib`: a group of files, named by its highest-ranked one.
    Area(String),
}

/// An anchor with the fingerprint it had when it was written: `sym:a::b#0123456789abcdef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub anchor: Anchor,
    pub fingerprint: Fingerprint,
}

const FLOW_STEP: &str = "->";

impl Index {
    /// The anchor's fingerprint now, or `None` when some item it names is gone.
    #[must_use]
    pub fn fingerprint(&self, anchor: &Anchor) -> Option<Fingerprint> {
        match anchor {
            Anchor::Symbol(path) => self.symbol(path).map(|symbol| symbol.fingerprint),
            Anchor::Flow(steps) => steps
                .iter()
                .map(|step| self.symbol(step).map(|symbol| symbol.fingerprint))
                .collect::<Option<Vec<_>>>()
                .map(Fingerprint::combine),
            Anchor::Area(id) => self.area(id).map(Area::fingerprint),
        }
    }
}

impl FromStr for Anchor {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        if let Some(path) = text.strip_prefix("sym:")
            && is_symbol_path(path)
        {
            return Ok(Self::Symbol(path.to_owned()));
        }
        if let Some(id) = text.strip_prefix("area:")
            && is_area_id(id)
        {
            return Ok(Self::Area(id.to_owned()));
        }
        let steps: Vec<&str> = text
            .strip_prefix("flow:")
            .ok_or(())?
            .split(FLOW_STEP)
            .collect();
        if steps.len() < 2 || !steps.iter().all(|step| is_symbol_path(step)) {
            return Err(());
        }
        Ok(Self::Flow(steps.into_iter().map(str::to_owned).collect()))
    }
}

fn is_area_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(char::is_whitespace)
}

fn is_symbol_path(path: &str) -> bool {
    let name = |ch: char| ch.is_alphanumeric() || matches!(ch, '_' | '[' | ']');
    path.split("::")
        .all(|part| !part.is_empty() && part.chars().all(name))
}

impl fmt::Display for Anchor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Symbol(path) => write!(formatter, "sym:{path}"),
            Self::Flow(steps) => write!(formatter, "flow:{}", steps.join(FLOW_STEP)),
            Self::Area(id) => write!(formatter, "area:{id}"),
        }
    }
}

impl FromStr for Pin {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        let (anchor, fingerprint) = text.rsplit_once('#').ok_or(())?;
        Ok(Self {
            anchor: anchor.parse()?,
            fingerprint: fingerprint.parse()?,
        })
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}#{}", self.anchor, self.fingerprint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index;
    use crate::test_support::file;

    fn repo(lib: &str) -> Index {
        index(&[
            file("a/Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("a/src/lib.rs", lib),
            file("a/src/m.rs", "pub struct S;\nimpl S { pub fn go() {} }\n"),
        ])
        .unwrap()
    }

    const LIB: &str = "mod m;\npub fn run() { m::S::go() }\n";

    #[test]
    fn finds_symbols_by_crate_module_and_owner() {
        let index = repo(LIB);
        for path in ["a_b::run", "a_b::m::S", "a_b::m::S::go"] {
            assert!(
                index
                    .fingerprint(&Anchor::Symbol(path.to_owned()))
                    .is_some(),
                "{path}"
            );
        }
        for path in [
            "a-b::run",
            "a_b::S",
            "a_b::m::run",
            "b::run",
            "a_b::m",
            "a_b::ru",
        ] {
            assert_eq!(
                index.fingerprint(&Anchor::Symbol(path.to_owned())),
                None,
                "{path}"
            );
        }
    }

    #[test]
    fn a_flow_depends_on_its_steps_and_their_order() {
        let index = repo(LIB);
        let fingerprint_of = |anchor: &str| index.fingerprint(&anchor.parse().unwrap()).unwrap();
        assert_ne!(
            fingerprint_of("flow:a_b::run->a_b::m::S::go"),
            fingerprint_of("flow:a_b::m::S::go->a_b::run")
        );
    }

    #[test]
    fn parses_and_prints_pins() {
        for text in [
            "sym:a::b#0123456789abcdef",
            "sym:a::B::c[2]#0123456789abcdef",
            "flow:a::b->c::d->e#0123456789abcdef",
            "area:crates/a-b/src/lib#0123456789abcdef",
            "area:my_apps/x+y@2:3/src/lib#0123456789abcdef",
        ] {
            assert_eq!(text.parse::<Pin>().unwrap().to_string(), text);
        }
    }

    #[test]
    fn rejects_malformed_pins() {
        for text in [
            "sym:a::b",
            "sym:a::b#0123",
            "sym:#0123456789abcdef",
            "sym:a::::b#0123456789abcdef",
            "sym:a b#0123456789abcdef",
            "sym:a#b#0123456789abcdef",
            "area:#0123456789abcdef",
            "area:a b#0123456789abcdef",
            "zone:x#0123456789abcdef",
            "flow:a::b#0123456789abcdef",
            "flow:a->#0123456789abcdef",
            "flow:a-->b#0123456789abcdef",
        ] {
            assert_eq!(text.parse::<Pin>(), Err(()), "{text}");
        }
    }
}
