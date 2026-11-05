use std::fmt;
use std::str::FromStr;

use crate::{Fingerprint, Index};

/// A place in the code that docs and tasks point at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Anchor {
    /// `sym:weacon_protocol::frame::encode_line`: one item.
    Sym(String),
    /// `flow:weacon_core::cli::run->weacon_core::daemon::serve`: items a flow passes through, in order.
    Flow(Vec<String>),
}

/// An anchor with the fingerprint it had when it was written: `sym:a::b#0123456789abcdef`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub anchor: Anchor,
    pub fingerprint: Fingerprint,
}

/// A pin that no longer matches the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    Missing(Anchor),
    /// The pin as it reads now.
    Changed(Pin),
    /// A line, counting from 1, that isn't a pin.
    Malformed(usize),
}

const FLOW_STEP: &str = "->";

impl Index {
    /// The anchor's fingerprint now, or `None` when some item it names is gone.
    #[must_use]
    pub fn fingerprint(&self, anchor: &Anchor) -> Option<Fingerprint> {
        match anchor {
            Anchor::Sym(path) => self.symbol(path).map(|symbol| symbol.fingerprint),
            Anchor::Flow(steps) => steps
                .iter()
                .map(|step| self.symbol(step).map(|symbol| symbol.fingerprint))
                .collect::<Option<Vec<_>>>()
                .map(Fingerprint::combine),
        }
    }

    /// Checks one pin per line, blank lines skipped, and lists the ones that no longer match.
    #[must_use]
    pub fn check(&self, pins: &str) -> Vec<Finding> {
        pins.lines()
            .map(str::trim)
            .enumerate()
            .filter(|(_, line)| !line.is_empty())
            .filter_map(|(number, line)| match line.parse::<Pin>() {
                Err(()) => Some(Finding::Malformed(number.saturating_add(1))),
                Ok(pin) => self.recheck(pin),
            })
            .collect()
    }

    fn recheck(&self, pin: Pin) -> Option<Finding> {
        match self.fingerprint(&pin.anchor) {
            None => Some(Finding::Missing(pin.anchor)),
            Some(now) if now == pin.fingerprint => None,
            Some(now) => Some(Finding::Changed(Pin {
                anchor: pin.anchor,
                fingerprint: now,
            })),
        }
    }
}

impl FromStr for Anchor {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, ()> {
        if let Some(path) = text.strip_prefix("sym:")
            && is_symbol_path(path)
        {
            return Ok(Self::Sym(path.to_owned()));
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

fn is_symbol_path(path: &str) -> bool {
    let name = |it: char| it.is_alphanumeric() || matches!(it, '_' | '[' | ']');
    path.split("::")
        .all(|part| !part.is_empty() && part.chars().all(name))
}

impl fmt::Display for Anchor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sym(path) => write!(formatter, "sym:{path}"),
            Self::Flow(steps) => write!(formatter, "flow:{}", steps.join(FLOW_STEP)),
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

impl fmt::Display for Finding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(anchor) => write!(formatter, "missing {anchor}"),
            Self::Changed(pin) => write!(formatter, "changed {pin}"),
            Self::Malformed(line) => write!(formatter, "malformed line {line}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SourceFile, index};

    fn repo(lib: &str) -> Index {
        let file = |path: &str, text: &str| SourceFile {
            path: path.to_owned(),
            text: text.to_owned(),
        };
        index(&[
            file("a/Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("a/src/lib.rs", lib),
            file("a/src/m.rs", "pub struct S;\nimpl S { pub fn go() {} }\n"),
        ])
        .unwrap()
    }

    fn pin(index: &Index, anchor: &str) -> String {
        let anchor: Anchor = anchor.parse().unwrap();
        let fingerprint = index.fingerprint(&anchor).unwrap();
        Pin {
            anchor,
            fingerprint,
        }
        .to_string()
    }

    const LIB: &str = "mod m;\npub fn run() { m::S::go() }\n";

    #[test]
    fn finds_symbols_by_crate_module_and_owner() {
        let index = repo(LIB);
        for path in ["a_b::run", "a_b::m::S", "a_b::m::S::go"] {
            assert!(
                index.fingerprint(&Anchor::Sym(path.to_owned())).is_some(),
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
                index.fingerprint(&Anchor::Sym(path.to_owned())),
                None,
                "{path}"
            );
        }
    }

    #[test]
    fn current_pins_have_no_findings() {
        let index = repo(LIB);
        let pins = format!(
            "{}\n\n  {}  \n",
            pin(&index, "sym:a_b::m::S::go"),
            pin(&index, "flow:a_b::run->a_b::m::S::go")
        );
        assert_eq!(index.check(&pins), []);
    }

    #[test]
    fn reports_missing_changed_and_malformed_pins() {
        let before = repo(LIB);
        let pins = [
            pin(&before, "sym:a_b::run"),
            pin(&before, "sym:a_b::m::S"),
            pin(&before, "flow:a_b::run->a_b::m::S::go"),
            "sym:a_b::gone#0000000000000000".to_owned(),
            "flow:a_b::m::S->a_b::gone#0000000000000000".to_owned(),
            "sym:a_b::run".to_owned(),
        ]
        .join("\n");

        let after = repo("mod m;\npub fn run() { m::S::go(); m::S::go() }\n");
        let changed = |anchor: &str| Finding::Changed(pin(&after, anchor).parse().unwrap());
        assert_eq!(
            after.check(&pins),
            [
                changed("sym:a_b::run"),
                changed("flow:a_b::run->a_b::m::S::go"),
                Finding::Missing(Anchor::Sym("a_b::gone".to_owned())),
                Finding::Missing("flow:a_b::m::S->a_b::gone".parse().unwrap()),
                Finding::Malformed(6),
            ]
        );
    }

    #[test]
    fn a_flow_depends_on_its_steps_and_their_order() {
        let index = repo(LIB);
        let print = |anchor: &str| index.fingerprint(&anchor.parse().unwrap()).unwrap();
        assert_ne!(
            print("flow:a_b::run->a_b::m::S::go"),
            print("flow:a_b::m::S::go->a_b::run")
        );
    }

    #[test]
    fn parses_and_prints_pins() {
        for text in [
            "sym:a::b#0123456789abcdef",
            "sym:a::B::c[2]#0123456789abcdef",
            "flow:a::b->c::d->e#0123456789abcdef",
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
            "area:x#0123456789abcdef",
            "flow:a::b#0123456789abcdef",
            "flow:a->#0123456789abcdef",
            "flow:a-->b#0123456789abcdef",
        ] {
            assert_eq!(text.parse::<Pin>(), Err(()), "{text}");
        }
    }

    #[test]
    fn prints_findings() {
        let pin: Pin = "sym:a::b#0123456789abcdef".parse().unwrap();
        assert_eq!(
            Finding::Missing(pin.anchor.clone()).to_string(),
            "missing sym:a::b"
        );
        assert_eq!(
            Finding::Changed(pin).to_string(),
            "changed sym:a::b#0123456789abcdef"
        );
        assert_eq!(Finding::Malformed(3).to_string(), "malformed line 3");
    }
}
