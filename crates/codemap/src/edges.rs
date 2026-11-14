use std::collections::{BTreeMap, BTreeSet};
use std::iter::once;

use crate::graph::{ModuleNode, absolute, longest_module, nodes, segments_of};
use crate::paths::{Bound, NamedPath};
use crate::refs::Ref;
use crate::symbols::{Item, Outline};
use crate::{Crate, Kind};

/// One item's code naming another item: `from` and `to` are paths such as `weacon_core::map::run`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Calls,
    Uses,
    /// From a type to a trait in the repository it implements.
    Implements,
    /// From a `#[test]` function, such as `a::m::tests::reads`, to what it calls or uses.
    Tests,
}

/// How many `use`s a path is followed through, re-exports included.
const HOPS: usize = 4;

/// The edges between the crates' items, sorted, each once; a name traced to no item gives none.
pub(crate) fn edges(
    crates: &[Crate],
    named_paths: &BTreeMap<&str, &[NamedPath]>,
    outlines: &BTreeMap<&str, &Outline>,
) -> Vec<Edge> {
    let items_of = |file: &str| {
        outlines
            .get(file)
            .map_or(&[][..], |outline| &outline.items[..])
    };
    let scope = Scope::new(crates, named_paths, &items_of);
    let mut found = BTreeSet::new();
    for ModuleNode { krate, module } in nodes(crates) {
        let Some(outline) = outlines.get(module.file.as_str()) else {
            continue;
        };
        let (krate, module) = (krate.name.as_str(), module.path.as_str());
        for item in &outline.items {
            found.extend(scope.edges(krate, module, item));
        }
        for test in &outline.tests {
            let edges = scope.edges(krate, module, test);
            found.extend(edges.into_iter().map(|edge| Edge {
                kind: EdgeKind::Tests,
                ..edge
            }));
        }
        for implemented in &outline.impls {
            let edges = scope.edges(krate, module, &implemented.as_item());
            let traits = edges
                .into_iter()
                .filter(|edge| scope.ids.get(&edge.to) == Some(&Kind::Trait));
            found.extend(traits.map(|edge| Edge {
                kind: EdgeKind::Implements,
                ..edge
            }));
        }
    }
    found.into_iter().collect()
}

/// `crate::module::Name`, leaving out an empty module.
pub(crate) fn id(krate: &str, module: &str, name: &str) -> String {
    join([krate, module, name])
}

fn join<'a>(parts: impl IntoIterator<Item = &'a str>) -> String {
    let parts: Vec<&str> = parts.into_iter().filter(|part| !part.is_empty()).collect();
    parts.join("::")
}

/// The module path of `within`, inline modules of `module`.
fn inside(module: &str, within: &[String]) -> String {
    join(once(module).chain(within.iter().map(String::as_str)))
}

struct Scope<'a> {
    crates: BTreeSet<&'a str>,
    /// Every item's path, and its kind.
    ids: BTreeMap<String, Kind>,
    /// The `use`s of every module, inline ones included, by crate and module path.
    imports: BTreeMap<(&'a str, String), Vec<&'a NamedPath>>,
    /// Methods by their own name, with the type they belong to.
    methods: BTreeMap<&'a str, Vec<(String, String)>>,
}

impl<'a> Scope<'a> {
    fn new(
        crates: &'a [Crate],
        named_paths: &BTreeMap<&str, &'a [NamedPath]>,
        items_of: &impl Fn(&str) -> &'a [Item],
    ) -> Self {
        let mut scope = Self {
            crates: crates.iter().map(|krate| krate.name.as_str()).collect(),
            ids: BTreeMap::new(),
            imports: BTreeMap::new(),
            methods: BTreeMap::new(),
        };
        for ModuleNode { krate, module } in nodes(crates) {
            let krate = krate.name.as_str();
            scope
                .imports
                .entry((krate, module.path.clone()))
                .or_default();
            let paths = named_paths
                .get(module.file.as_str())
                .copied()
                .unwrap_or_default();
            for path in paths.iter().filter(|path| path.bound.is_some()) {
                let key = (krate, inside(&module.path, &path.within));
                scope.imports.entry(key).or_default().push(path);
            }
            for item in items_of(&module.file) {
                scope.add(krate, &module.path, item);
            }
        }
        scope
    }

    fn add(&mut self, krate: &'a str, module: &str, item: &'a Item) {
        let symbol = &item.symbol;
        let path = id(krate, module, &symbol.name);
        if symbol.kind == Kind::Fn
            && let Some(owner) = &item.owner
            && let Some((_, method)) = symbol.name.rsplit_once("::")
        {
            let owner = id(krate, &inside(module, &item.within), owner);
            let entry = self.methods.entry(method).or_default();
            entry.push((owner, path.clone()));
        }
        self.ids.insert(path, symbol.kind);
    }

    fn edges(&self, krate: &str, module: &str, item: &Item) -> Vec<Edge> {
        let from = id(krate, module, &item.symbol.name);
        let mut found: Vec<(String, EdgeKind)> = item
            .refs
            .iter()
            .filter_map(|named| {
                let (path, kind) = match named {
                    Ref::Call(path) => (path, EdgeKind::Calls),
                    Ref::Use(path) => (path, EdgeKind::Uses),
                    Ref::Method(_) => return None,
                };
                let (to, exact) = self.path(krate, module, item, path)?;
                Some((to, if exact { kind } else { EdgeKind::Uses }))
            })
            .collect();
        let mut known: BTreeSet<String> = found.iter().map(|(to, _)| to.clone()).collect();
        let here = inside(module, &item.within);
        known.extend(item.owner.as_deref().map(|owner| id(krate, &here, owner)));
        found.extend(item.refs.iter().filter_map(|named| match named {
            Ref::Method(name) => Some((self.method(name, &known)?, EdgeKind::Calls)),
            Ref::Call(_) | Ref::Use(_) => None,
        }));
        found
            .into_iter()
            .filter(|(to, _)| *to != from)
            .map(|(to, kind)| Edge {
                from: from.clone(),
                to,
                kind,
            })
            .collect()
    }

    /// The item a path written in `item` names, and whether it names that item itself rather
    /// than something inside it that isn't an item, such as an enum's variant.
    fn path(
        &self,
        krate: &str,
        module: &str,
        item: &Item,
        path: &[String],
    ) -> Option<(String, bool)> {
        let segments = path
            .iter()
            .map(|segment| match (segment.as_str(), &item.owner) {
                ("Self", Some(owner)) => owner.as_str(),
                (segment, _) => segment,
            });
        let mut here = segments_of(module);
        here.extend(item.within.iter().map(String::as_str));
        let (krate, segments) = absolute(&self.crates, krate, here, segments);
        if let Some(exact) = self.find(krate, &segments, HOPS) {
            return Some((exact, true));
        }
        let within = (1..segments.len()).rev().find_map(|length| {
            let found = self.find(krate, segments.get(..length)?, HOPS)?;
            (self.ids.get(&found) != Some(&Kind::Mod)).then_some(found)
        });
        within.map(|found| (found, false))
    }

    fn find(&self, krate: &str, segments: &[&str], hops: usize) -> Option<String> {
        let item = join(once(krate).chain(segments.iter().copied()));
        if self.ids.contains_key(&item) {
            return Some(item);
        }
        let hops = hops.checked_sub(1)?;
        let (_, before_name) = segments.split_last()?;
        let length = longest_module(before_name, usize::MAX, |module| {
            self.imports.contains_key(&(krate, module.to_owned()))
        })?;
        let (module, name, rest) = (
            segments.get(..length)?,
            segments.get(length)?,
            segments.get(length + 1..)?,
        );
        let imports = self.imports.get(&(krate, module.join("::")))?;
        let through = |import: &&NamedPath, name: Option<&str>| {
            let written = import.segments.iter().map(String::as_str);
            let segments = written.chain(name).chain(rest.iter().copied());
            let (krate, segments) = absolute(&self.crates, krate, module.to_vec(), segments);
            self.find(krate, &segments, hops)
        };
        // A name brought in by name hides what a glob brings in under it.
        let by_name = imports
            .iter()
            .find(|import| matches!(&import.bound, Some(Bound::Name(bound)) if bound == name));
        if let Some(import) = by_name {
            return through(import, None);
        }
        imports
            .iter()
            .filter(|import| import.bound == Some(Bound::Glob))
            .find_map(|import| through(import, Some(name)))
    }

    /// The one method named `name` whose type is among `known`, the types its caller names.
    fn method(&self, name: &str, known: &BTreeSet<String>) -> Option<String> {
        let mut found = self
            .methods
            .get(name)?
            .iter()
            .filter(|(owner, _)| known.contains(owner));
        let (_, id) = found.next()?;
        found.next().is_none().then(|| id.clone())
    }
}

#[cfg(test)]
mod tests {
    use crate::index;
    use crate::test_support::files;

    fn edges(list: &[(&str, &str)]) -> Vec<String> {
        index(&files(list))
            .unwrap()
            .edges
            .into_iter()
            .map(|edge| format!("{} {:?} {}", edge.from, edge.kind, edge.to))
            .collect()
    }

    const CRATE_A: &str = "[package]\nname = \"a\"\n";
    const CRATE_B: &str = "[package]\nname = \"b\"\n";

    #[test]
    fn follows_paths_imports_and_self() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "mod m;\nuse m::Frame as F;\nuse crate::m::*;\n\
                 pub fn run(f: F) { m::go(); helper(); Frame::new(); later(); std::fs::read(); }\n\
                 fn helper() {}\n",
            ),
            (
                "a/src/m.rs",
                "pub struct Frame;\nimpl Frame { pub fn new() -> Self { Self::make() } fn make() -> Self { Frame } }\n\
                 pub fn go() {}\npub fn later() {}\n",
            ),
        ]);
        assert_eq!(
            found,
            [
                "a::m::Frame::make Uses a::m::Frame",
                "a::m::Frame::new Uses a::m::Frame",
                "a::m::Frame::new Calls a::m::Frame::make",
                "a::run Calls a::helper",
                "a::run Uses a::m::Frame",
                "a::run Calls a::m::Frame::new",
                "a::run Calls a::m::go",
                "a::run Calls a::m::later",
            ]
        );
    }

    #[test]
    fn follows_re_exports_across_crates() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            ("a/src/lib.rs", "mod inner;\npub use inner::Thing;\n"),
            ("a/src/inner.rs", "pub struct Thing;\n"),
            ("b/Cargo.toml", CRATE_B),
            (
                "b/src/lib.rs",
                "use a::Thing;\nfn take(_: Thing, _: a::Thing) {}\n",
            ),
        ]);
        assert_eq!(found, ["b::take Uses a::inner::Thing"]);
    }

    #[test]
    fn calls_a_method_of_a_type_the_caller_names() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "struct X;\nstruct Y;\nimpl X { fn once(&self) { self.twice(); } fn twice(&self) {} }\n\
                 impl Y { fn once(&self) {} fn twice(&self) {} }\n\
                 fn one(x: X) { x.once(); x.len(); }\nfn both(x: X, y: Y) { x.twice(); }\n\
                 fn none(x: u8) { x.once(); }\n",
            ),
        ]);
        assert_eq!(
            found,
            [
                "a::X::once Calls a::X::twice",
                "a::both Uses a::X",
                "a::both Uses a::Y",
                "a::one Uses a::X",
                "a::one Calls a::X::once",
            ]
        );
    }

    #[test]
    fn resolves_from_inline_modules() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "pub fn g() {}\npub mod b {\n use super::g as top;\n pub fn g() {}\n\
                 pub fn f() { g(); top(); self::g(); super::g(); }\n pub struct F;\n\
                 impl F { pub fn new() -> Self { Self::make() } fn make() -> Self { F } }\n}\n",
            ),
        ]);
        assert_eq!(
            found,
            [
                "a::b::F::make Uses a::b::F",
                "a::b::F::new Uses a::b::F",
                "a::b::F::new Calls a::b::F::make",
                "a::b::f Calls a::b::g",
                "a::b::f Calls a::g",
            ]
        );
    }

    #[test]
    fn follows_the_imports_of_a_module_below_the_root() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            ("a/src/lib.rs", "mod m;\npub fn top() {}\n"),
            ("a/src/m.rs", "use crate::top;\npub fn f() { top(); }\n"),
        ]);
        assert_eq!(found, ["a::m::f Calls a::top"]);
    }

    #[test]
    fn self_in_a_trait_names_the_trait() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "pub trait C { fn d(&self) { Self::e(); } fn e(); }\n\
                 pub mod m { pub fn f() { Self::g(); } pub fn g() {} }\n",
            ),
        ]);
        assert_eq!(found, ["a::C::d Calls a::C::e"]);
    }

    #[test]
    fn a_use_by_name_hides_a_glob() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "mod n;\nuse n::*;\nuse std::fmt::Result;\npub fn f(_: Result, _: Other) {}\n",
            ),
            ("a/src/n.rs", "pub struct Result;\npub struct Other;\n"),
        ]);
        assert_eq!(found, ["a::f Uses a::n::Other"]);
    }

    #[test]
    fn a_path_inside_an_item_uses_the_item() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "pub enum K { X }\npub fn g() -> K { K::X }\npub fn h() { K::default(); }\n",
            ),
        ]);
        assert_eq!(found, ["a::g Uses a::K", "a::h Uses a::K"]);
    }

    #[test]
    fn links_types_to_traits_and_tests_to_what_they_test() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "mod m;\npub trait Show {}\npub struct X;\nimpl Show for X {}\n\
                 impl std::fmt::Debug for X {}\nimpl<T> From<T> for X {}\n\
                 pub fn run() {}\n#[cfg(test)]\nmod tests {\n use super::*;\n\
                 fn helper() { run(); }\n #[test]\n fn runs() { run(); m::go(); let _x: X; }\n\
                 mod deeper { #[tokio::test]\n async fn shows() { crate::m::Y::show(); } }\n}\n",
            ),
            (
                "a/src/m.rs",
                "pub fn go() {}\npub struct Y;\nmod inner { pub trait Shown {} }\n\
                 impl Y { pub fn show() {} }\nimpl inner::Shown for Y {}\n",
            ),
        ]);
        assert_eq!(
            found,
            [
                "a::X Implements a::Show",
                "a::m::Y Implements a::m::inner::Shown",
                "a::tests::deeper::shows Tests a::m::Y::show",
                "a::tests::runs Tests a::X",
                "a::tests::runs Tests a::m::go",
                "a::tests::runs Tests a::run",
            ]
        );
    }

    #[test]
    fn gives_up_on_import_loops() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            ("a/src/lib.rs", "mod m;\nuse m::X;\nfn run(_: X) {}\n"),
            ("a/src/m.rs", "pub use crate::X;\n"),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }
}
