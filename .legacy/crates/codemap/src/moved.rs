use crate::index::Located;
use crate::{Anchor, Fingerprint, Index, Pin};

/// What a move of the code did to a pin whose anchor named something before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moved {
    /// Nothing a doc says changed, but the pin must read as this: its symbol was renamed or
    /// moved, or its area was recomputed.
    Reanchored(Pin),
    /// Its code changed, from what `was` describes to what `now` does: `fn a::run  a/src/lib.rs:3`.
    Changed { was: String, now: String },
    /// It names nothing any more.
    Gone { was: String },
}

impl Index {
    /// What moving the code from `self` to `now` did to `pin`; `None` when its anchor named
    /// nothing in `self`, as in another repository's code, or when the move left it as it was.
    /// A symbol is re-anchored only when the pin matched it before the move and it was renamed
    /// or moved with its code unchanged, to the one item that could be it; one that another item
    /// of its kind with other code replaced in its file has changed, to that item.
    #[must_use]
    pub fn after_move(&self, now: &Self, pin: &Pin) -> Option<Moved> {
        let before = self.fingerprint(&pin.anchor)?;
        let was = self.describe(&pin.anchor)?;
        match now.fingerprint(&pin.anchor) {
            Some(after) if after == before => None,
            Some(fingerprint) if matches!(pin.anchor, Anchor::Area(_)) => {
                Some(Moved::Reanchored(Pin {
                    anchor: pin.anchor.clone(),
                    fingerprint,
                }))
            }
            Some(_) => Some(Moved::Changed {
                now: now.describe(&pin.anchor)?,
                was,
            }),
            None => Some(self.unnamed(now, pin, before, was)),
        }
    }

    /// What became of `pin`, which named something that `before` fingerprints and `was`
    /// describes, now that its anchor names nothing in `now`.
    fn unnamed(&self, now: &Self, pin: &Pin, before: Fingerprint, was: String) -> Moved {
        let reanchored = self
            .renamed(now, &pin.anchor)
            .filter(|_| pin.fingerprint == before)
            .and_then(|anchor| {
                let fingerprint = now.fingerprint(&anchor)?;
                Some(Moved::Reanchored(Pin {
                    anchor,
                    fingerprint,
                }))
            });
        let replaced = || {
            let Anchor::Symbol(path) = &pin.anchor else {
                return None;
            };
            let new = self.replaced(now, path)?;
            let now = now.items.get(&new)?.describe(&new);
            Some(Moved::Changed {
                was: was.clone(),
                now,
            })
        };
        reanchored.or_else(replaced).unwrap_or(Moved::Gone { was })
    }

    /// What `anchor` names in `self`, in a line for each item.
    fn describe(&self, anchor: &Anchor) -> Option<String> {
        match anchor {
            Anchor::Symbol(path) => Some(self.items.get(path)?.describe(path)),
            Anchor::Flow(steps) => steps
                .iter()
                .map(|step| Some(self.items.get(step)?.describe(step)))
                .collect::<Option<Vec<_>>>()
                .map(|lines| lines.join("\n")),
            Anchor::Area(id) => {
                let files = self.area(id)?.files.len();
                Some(format!("area {id}  {files} files"))
            }
        }
    }

    /// What `anchor`, which names nothing in `now`, was renamed to there.
    fn renamed(&self, now: &Self, anchor: &Anchor) -> Option<Anchor> {
        match anchor {
            Anchor::Symbol(path) => self.moved_symbol(now, path).map(Anchor::Symbol),
            Anchor::Flow(steps) => steps
                .iter()
                .map(|step| match now.items.get(step) {
                    // A step whose code changed is no mechanical re-anchor.
                    Some(item) => (item.symbol.fingerprint
                        == self.items.get(step)?.symbol.fingerprint)
                        .then(|| step.clone()),
                    None => self.moved_symbol(now, step),
                })
                .collect::<Option<Vec<_>>>()
                .map(Anchor::Flow),
            // An area is named by its top file, so it is the area that file is in now.
            Anchor::Area(id) => {
                let top = self.area(id)?.files.first()?;
                let area = now.areas.iter().find(|area| area.files.contains(top))?;
                Some(Anchor::Area(area.id.clone()))
            }
        }
    }

    /// The one path in `now` that the item at `path` moved to with its code unchanged, else the
    /// one it was renamed to in its file: the one item of its shape there that arrived, as the
    /// one of its shape there left.
    fn moved_symbol(&self, now: &Self, path: &str) -> Option<String> {
        let old = self.items.get(path)?;
        let fingerprint = old.symbol.fingerprint;
        let moved: Vec<&String> = self
            .arrived(now)
            .filter(|(_, item)| item.symbol.fingerprint == fingerprint)
            .map(|(path, _)| path)
            .collect();
        if let [moved] = moved.as_slice() {
            return Some((*moved).clone());
        }
        self.swapped(now, |item| {
            item.file == old.file && item.symbol.shape == old.symbol.shape
        })
    }

    /// The one item of its kind that arrived in the file of the item at `path` as that left.
    fn replaced(&self, now: &Self, path: &str) -> Option<String> {
        let old = self.items.get(path)?;
        self.swapped(now, |item| {
            item.file == old.file && item.symbol.kind == old.symbol.kind
        })
    }

    /// The path of the one `alike` item that arrived in `now`, when one `alike` item left.
    fn swapped(&self, now: &Self, alike: impl Fn(&Located) -> bool) -> Option<String> {
        let left = self
            .items
            .iter()
            .filter(|(path, item)| alike(item) && !now.items.contains_key(*path))
            .count();
        let arrived: Vec<&String> = self
            .arrived(now)
            .filter(|(_, item)| alike(item))
            .map(|(path, _)| path)
            .collect();
        match arrived.as_slice() {
            [arrived] if left == 1 => Some((*arrived).clone()),
            _ => None,
        }
    }

    /// The items in `now` that aren't in `self`.
    fn arrived<'a>(&'a self, now: &'a Self) -> impl Iterator<Item = (&'a String, &'a Located)> {
        now.items
            .iter()
            .filter(|(path, _)| !self.items.contains_key(*path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index;
    use crate::test_support::file;

    fn repo(lib: &str, module: &str) -> Index {
        index(&[
            file("a/Cargo.toml", "[package]\nname = \"a-b\"\n"),
            file("a/src/lib.rs", lib),
            file("a/src/m.rs", module),
        ])
        .unwrap()
    }

    const LIB: &str = "mod m;\npub fn run() { m::go() }\n";
    const MODULE: &str = "pub fn go() {}\npub struct S;\n";

    fn pin(index: &Index, anchor: &str) -> Pin {
        let anchor: Anchor = anchor.parse().unwrap();
        let fingerprint = index.fingerprint(&anchor).unwrap();
        Pin {
            anchor,
            fingerprint,
        }
    }

    fn moved(before: &Index, after: &Index, anchor: &str) -> Option<Moved> {
        before.after_move(after, &pin(before, anchor))
    }

    #[test]
    fn a_pin_the_move_left_alone_or_that_named_nothing_has_nothing() {
        let before = repo(LIB, MODULE);
        let after = repo(LIB, "pub fn go() {}\npub struct S;\npub fn more() {}\n");
        let elsewhere = "sym:other::go#0123456789abcdef".parse().unwrap();

        assert_eq!(moved(&before, &after, "sym:a_b::m::go"), None);
        assert_eq!(moved(&before, &after, "flow:a_b::run->a_b::m::go"), None);
        assert_eq!(before.after_move(&after, &elsewhere), None);
    }

    #[test]
    fn a_symbol_renamed_or_moved_with_its_code_unchanged_is_reanchored() {
        let before = repo(LIB, MODULE);
        let renamed = repo(
            "mod m;\npub fn run() { m::went() }\n",
            "pub fn went() {}\npub struct S;\n",
        );
        let moved_out = repo(
            "mod m;\npub fn run() { go() }\npub fn go() {}\n",
            "pub struct S;\n",
        );

        assert_eq!(
            moved(&before, &renamed, "sym:a_b::m::go"),
            Some(Moved::Reanchored(pin(&renamed, "sym:a_b::m::went")))
        );
        assert_eq!(
            moved(&before, &moved_out, "sym:a_b::m::go"),
            Some(Moved::Reanchored(pin(&moved_out, "sym:a_b::go")))
        );
        // Its caller's code changed with the call, so the flow is no mechanical re-anchor.
        assert!(matches!(
            moved(&before, &renamed, "flow:a_b::run->a_b::m::go"),
            Some(Moved::Gone { .. })
        ));
    }

    #[test]
    fn a_symbol_that_changed_or_went_says_what_it_was_and_is() {
        let before = repo(LIB, MODULE);
        let changed = repo(LIB, "pub fn go() { let x = 1; }\npub struct S;\n");
        let gone = repo("mod m;\npub fn run() {}\n", "pub struct S;\n");

        assert_eq!(
            moved(&before, &changed, "sym:a_b::m::go"),
            Some(Moved::Changed {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned(),
                now: "fn a_b::m::go  a/src/m.rs:1".to_owned(),
            })
        );
        assert_eq!(
            moved(&before, &gone, "sym:a_b::m::go"),
            Some(Moved::Gone {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned()
            })
        );
        assert_eq!(
            moved(&before, &changed, "flow:a_b::run->a_b::m::go"),
            Some(Moved::Changed {
                was: "fn a_b::run  a/src/lib.rs:2\nfn a_b::m::go  a/src/m.rs:1".to_owned(),
                now: "fn a_b::run  a/src/lib.rs:2\nfn a_b::m::go  a/src/m.rs:1".to_owned(),
            })
        );
    }

    #[test]
    fn a_replacement_is_named_only_when_one_item_could_be_it() {
        let before = repo(LIB, MODULE);
        let two = repo(
            "mod m;\npub fn run() {}\n",
            "pub fn a() {}\npub fn b() {}\npub struct S;\n",
        );
        let pair = repo(LIB, "pub fn go() {}\npub fn other() {}\n");
        let both_left = pair.after_move(
            &repo("mod m;\npub fn run() {}\n", "pub fn c() {}\n"),
            &pin(&pair, "sym:a_b::m::go"),
        );
        let stale = Pin {
            fingerprint: "0123456789abcdef".parse().unwrap(),
            ..pin(&before, "sym:a_b::m::go")
        };
        let renamed = repo(
            "mod m;\npub fn run() { m::went() }\n",
            "pub fn went() {}\npub struct S;\n",
        );
        let gone = |was: &str| {
            Some(Moved::Gone {
                was: was.to_owned(),
            })
        };

        assert_eq!(
            moved(&before, &two, "sym:a_b::m::go"),
            gone("fn a_b::m::go  a/src/m.rs:1")
        );
        assert_eq!(both_left, gone("fn a_b::m::go  a/src/m.rs:1"));
        assert_eq!(
            before.after_move(&renamed, &stale),
            Some(Moved::Changed {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned(),
                now: "fn a_b::m::went  a/src/m.rs:1".to_owned(),
            })
        );
        assert_eq!(
            moved(&before, &renamed, "flow:a_b::m::go->a_b::m::S"),
            Some(Moved::Reanchored(pin(
                &renamed,
                "flow:a_b::m::went->a_b::m::S"
            )))
        );
        assert_eq!(
            moved(&before, &two, "flow:a_b::run->a_b::m::go"),
            gone("fn a_b::run  a/src/lib.rs:2\nfn a_b::m::go  a/src/m.rs:1")
        );
    }

    fn areas(lib: &str, x: &str) -> Index {
        index(&[
            file("a/Cargo.toml", "[package]\nname = \"a\"\n"),
            file("a/src/lib.rs", lib),
            file("a/src/x.rs", x),
            file(
                "a/src/y.rs",
                "pub fn g() { crate::x::f(); crate::top(); }\n",
            ),
        ])
        .unwrap()
    }

    #[test]
    fn a_recomputed_area_is_reanchored_and_one_whose_files_went_is_gone() {
        let lib = "mod x;\nmod y;\npub fn top() { x::f(); y::g(); }\n";
        let before = areas(lib, "pub fn f() { crate::y::g(); crate::top(); }\n");
        let id = format!("area:{}", before.areas[0].id);
        let mut regrouped = before.clone();
        regrouped.areas[0].files.pop();
        let mut renamed = before.clone();
        renamed.areas[0].id = "a/src/elsewhere".to_owned();
        let mut emptied = before.clone();
        emptied.areas.clear();

        assert_eq!(
            moved(&before, &regrouped, &id),
            Some(Moved::Reanchored(pin(&regrouped, &id)))
        );
        assert_eq!(
            moved(&before, &renamed, &id),
            Some(Moved::Reanchored(pin(&renamed, "area:a/src/elsewhere")))
        );
        let files = before.areas[0].files.len();
        assert_eq!(
            moved(&before, &emptied, &id),
            Some(Moved::Gone {
                was: format!("{}  {files} files", id.replacen(':', " ", 1))
            })
        );
    }

    #[test]
    fn a_replacement_is_an_item_of_its_kind_in_its_file_whatever_else_left() {
        let before = repo("mod m;\npub fn run() { m::go() }\npub struct X;\n", MODULE);
        let other_kind = repo(LIB, "pub struct W(u8);\npub struct S;\n");
        let elsewhere_left = repo(
            "mod m;\npub fn run() { m::went() }\n",
            "pub fn went() {}\npub struct S;\n",
        );

        assert_eq!(
            moved(&before, &other_kind, "sym:a_b::m::go"),
            Some(Moved::Gone {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned()
            })
        );
        assert_eq!(
            moved(&before, &elsewhere_left, "sym:a_b::m::go"),
            Some(Moved::Reanchored(pin(&elsewhere_left, "sym:a_b::m::went")))
        );
    }

    #[test]
    fn a_flow_whose_other_step_stayed_as_one_moved_unchanged_is_reanchored() {
        let before = repo(
            "mod m;\npub fn run() {}\n",
            "pub fn go() {}\npub struct S;\n",
        );
        let after = repo(
            "mod m;\npub fn run() {}\npub fn go() {}\n",
            "pub struct S;\n",
        );

        let Some(Moved::Reanchored(now)) = moved(&before, &after, "flow:a_b::run->a_b::m::go")
        else {
            panic!("not reanchored");
        };
        assert_eq!(now.anchor.to_string(), "flow:a_b::run->a_b::go");
    }

    #[test]
    fn a_flow_whose_other_step_changed_as_one_moved_is_no_reanchor() {
        let before = repo(LIB, MODULE);
        let after = repo(
            "mod m;\npub fn run() { m::went(); m::went() }\n",
            "pub fn went() {}\npub struct S;\n",
        );

        assert_eq!(
            moved(&before, &after, "flow:a_b::run->a_b::m::go"),
            Some(Moved::Gone {
                was: "fn a_b::run  a/src/lib.rs:2\nfn a_b::m::go  a/src/m.rs:1".to_owned()
            })
        );
    }

    #[test]
    fn a_replacement_whose_code_differs_needs_an_update_and_is_never_reanchored() {
        let one = repo(
            "mod m;\npub fn run() { m::go(); }\n",
            "pub fn go() -> u8 { 1 }\n",
        );
        let two = repo(
            "mod m;\npub fn run() { m::went(); }\n",
            "pub fn went() -> u8 { 2 }\n",
        );

        assert_eq!(
            moved(&one, &two, "sym:a_b::m::go"),
            Some(Moved::Changed {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned(),
                now: "fn a_b::m::went  a/src/m.rs:1".to_owned(),
            })
        );
    }
    #[test]
    fn a_symbol_renamed_as_it_moved_to_another_file_is_gone() {
        let before = repo(LIB, MODULE);
        let after = repo(
            "mod m;\npub fn run() { went() }\npub fn went() {}\n",
            "pub struct S;\n",
        );

        assert_eq!(
            moved(&before, &after, "sym:a_b::m::go"),
            Some(Moved::Gone {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned()
            })
        );
    }

    #[test]
    fn items_moved_with_their_module_are_reanchored_beside_others_of_their_shape() {
        let lib = "pub mod m { pub fn go() {} pub fn stop() {} }\n";
        let before = repo(lib, "");
        let after = repo("pub mod n { pub fn go() {} pub fn stop() {} }\n", "");

        assert_eq!(
            moved(&before, &after, "sym:a_b::m::go"),
            Some(Moved::Reanchored(pin(&after, "sym:a_b::n::go")))
        );
    }

    #[test]
    fn a_rename_that_more_than_one_item_could_be_is_no_reanchor() {
        let before = repo(LIB, MODULE);
        let two_arrived = repo(
            "mod m;\npub fn run() { m::a() }\n",
            "pub fn a() {}\npub fn b() {}\npub struct S;\n",
        );
        let pair = repo(LIB, "pub fn go() {}\npub fn other() {}\n");
        let one_arrived = repo(LIB, "pub fn went() {}\n");

        assert_eq!(
            moved(&before, &two_arrived, "sym:a_b::m::go"),
            Some(Moved::Gone {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned()
            })
        );
        // Both left and one arrived, so it may be either.
        assert_eq!(
            moved(&pair, &one_arrived, "sym:a_b::m::go"),
            Some(Moved::Gone {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned()
            })
        );
    }

    #[test]
    fn a_rename_that_also_changed_the_code_elsewhere_is_gone() {
        let before = repo(LIB, MODULE);
        let after = repo(
            "mod m;\npub fn run() { went() }\npub fn went() -> u8 { 1 }\n",
            "pub struct S;\n",
        );

        assert_eq!(
            moved(&before, &after, "sym:a_b::m::go"),
            Some(Moved::Gone {
                was: "fn a_b::m::go  a/src/m.rs:1".to_owned()
            })
        );
    }
}
