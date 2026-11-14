use std::collections::{BTreeMap, BTreeSet};
use std::iter::once;

use crate::graph::{ModuleNode, absolute, longest_module, nodes, segments_of};
use crate::paths::{Bound, NamedPath, Segments};
use crate::refs::Ref;
use crate::symbols::{Impl, Item, Outline, items_in};
use crate::{Crate, Kind};

/// One item's code naming another item: `from` and `to` are paths such as `weacon_core::map::run`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Edge {
    pub(crate) from: String,
    pub(crate) to: String,
    pub(crate) kind: EdgeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum EdgeKind {
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
    let items_of = |file: &str| items_in(outlines, file);
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
            found.extend(scope.implements(krate, module, implemented));
        }
    }
    found.into_iter().collect()
}

/// A method, before its type is resolved.
struct Method<'a> {
    krate: &'a str,
    /// The module its type is written in.
    here: String,
    owner: &'a str,
    name: &'a str,
    path: String,
}

fn method<'a>(krate: &'a str, module: &str, item: &'a Item, path: String) -> Option<Method<'a>> {
    let owner = item.owner.as_deref()?;
    let (_, name) = item.symbol.name.rsplit_once("::")?;
    (item.symbol.kind == Kind::Fn).then(|| Method {
        krate,
        here: inside(module, &item.within),
        owner,
        name,
        path,
    })
}

/// Where a path is written: its crate, its module, the inline modules around it and the type
/// `Self` names there.
struct Spot<'s> {
    krate: &'s str,
    module: &'s str,
    within: &'s [String],
    owner: Option<&'s str>,
    /// What `use`s there bring into scope beyond the module's own.
    imports: &'s [(Segments, Bound)],
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
    /// How deep the deepest module is, so no longer start of a path is tried.
    deepest: usize,
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
            deepest: 0,
        };
        let mut methods = Vec::new();
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
                let path = id(krate, &module.path, &item.symbol.name);
                scope.ids.insert(path.clone(), item.symbol.kind);
                methods.extend(method(krate, &module.path, item, path));
            }
        }
        scope.deepest = scope
            .imports
            .keys()
            .map(|(_, module)| segments_of(module).len())
            .max()
            .unwrap_or(0);
        // Owners resolve only once every item and import is known.
        for method in methods {
            let owner = scope.owner(method.krate, &method.here, method.owner);
            let entry = scope.methods.entry(method.name).or_default();
            entry.push((owner, method.path));
        }
        scope
    }

    /// The path of the type that `owner`, as written in module `here`, names.
    fn owner(&self, krate: &str, here: &str, owner: &str) -> String {
        let (found_in, segments) = absolute(&self.crates, krate, segments_of(here), [owner]);
        self.find(found_in, &segments, HOPS)
            .unwrap_or_else(|| id(krate, here, owner))
    }

    /// The edge from an `impl Trait for Type`'s type to its trait, when the trait is in the repository.
    fn implements(&self, krate: &str, module: &str, implemented: &Impl) -> Option<Edge> {
        let spot = Spot {
            krate,
            module,
            within: &implemented.within,
            owner: Some(&implemented.owner),
            imports: &[],
        };
        let (to, true) = self.path(&spot, &implemented.trait_path)? else {
            return None;
        };
        let (from, true) = self.path(&spot, &implemented.owner_path)? else {
            return None;
        };
        (self.ids.get(&to) == Some(&Kind::Trait)).then_some(Edge {
            from,
            to,
            kind: EdgeKind::Implements,
        })
    }

    fn edges(&self, krate: &str, module: &str, item: &Item) -> Vec<Edge> {
        let from = id(krate, module, &item.symbol.name);
        let spot = Spot {
            krate,
            module,
            within: &item.within,
            owner: item.owner.as_deref(),
            imports: &item.imports,
        };
        let mut found: Vec<(String, EdgeKind)> = item
            .refs
            .iter()
            .filter_map(|named| {
                let (path, kind) = match named {
                    Ref::Call(path) => (path, EdgeKind::Calls),
                    Ref::Use(path) => (path, EdgeKind::Uses),
                    Ref::Method(_) => return None,
                };
                let (to, exact) = self.path(&spot, path)?;
                Some((to, if exact { kind } else { EdgeKind::Uses }))
            })
            .collect();
        let mut known: BTreeSet<String> = found.iter().map(|(to, _)| to.clone()).collect();
        let here = inside(module, &item.within);
        known.extend(
            item.owner
                .as_deref()
                .map(|owner| self.owner(krate, &here, owner)),
        );
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
    /// The item a path written at `spot` names, through the `use`s there first: one that binds
    /// its first name, else the module's own names, else what a glob there brings in.
    fn path(&self, spot: &Spot<'_>, path: &[String]) -> Option<(String, bool)> {
        let first = path.first()?;
        let bound = spot
            .imports
            .iter()
            .find(|(_, bound)| matches!(bound, Bound::Name(name) if name == first));
        if let Some((segments, _)) = bound {
            return self.resolve(spot, &[&segments[..], path.get(1..)?].concat());
        }
        self.resolve(spot, path).or_else(|| {
            let globs = spot
                .imports
                .iter()
                .filter(|(_, bound)| *bound == Bound::Glob);
            globs
                .into_iter()
                .find_map(|(segments, _)| self.resolve(spot, &[&segments[..], path].concat()))
        })
    }

    fn resolve(&self, spot: &Spot<'_>, path: &[String]) -> Option<(String, bool)> {
        let segments = path
            .iter()
            .map(|segment| match (segment.as_str(), spot.owner) {
                ("Self", Some(owner)) => owner,
                (segment, _) => segment,
            });
        let mut here = segments_of(spot.module);
        here.extend(spot.within.iter().map(String::as_str));
        let (krate, segments) = absolute(&self.crates, spot.krate, here, segments);
        if let Some(exact) = self.find(krate, &segments, HOPS) {
            return Some((exact, true));
        }
        // `model::X::run` names the method its type's `impl` declares, wherever that is written.
        if let Some((name, owner)) = segments.split_last()
            && let Some(owner) = self.find(krate, owner, HOPS)
            && let Some((_, method)) = self
                .methods
                .get(name)
                .and_then(|methods| methods.iter().find(|(of, _)| *of == owner))
        {
            return Some((method.clone(), true));
        }
        // Only what was written is dropped from the end: never the module it was written in.
        let shortest = (segments.len() + 1).saturating_sub(path.len()).max(1);
        let within = (shortest..segments.len()).rev().find_map(|length| {
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
        let length = longest_module(before_name, self.deepest, |module| {
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
                "mod m;\npub trait Show {}\npub trait Wrap<T> {}\npub struct X;\nimpl Show for X {}\n\
                 impl Wrap<u8> for X {}\n\
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
                "a::X Implements a::Wrap",
                "a::m::Y Implements a::m::inner::Shown",
                "a::tests::deeper::shows Tests a::m::Y::show",
                "a::tests::runs Tests a::X",
                "a::tests::runs Tests a::m::go",
                "a::tests::runs Tests a::run",
            ]
        );
    }

    #[test]
    fn a_name_alone_does_not_fall_back_to_its_module() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            ("a/src/lib.rs", "mod index;\npub use index::index;\n"),
            (
                "a/src/index.rs",
                "pub fn index() {}\npub struct File { text: String }\n",
            ),
        ]);
        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn resolves_what_code_review_found_missing() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "mod model;\nuse model::X;\nimpl X { fn run(&self) {} }\nfn caller(x: X) { x.run(); }\n\
                 const LIMIT: u8 = 1;\nfn limit() -> u8 { LIMIT }\n\
                 pub struct Store<A>(A);\nimpl<A> Store<A> { fn new() {} }\nfn make() { Store::<u8>::new(); }\n\
                 pub trait Base {}\npub trait Derived: Base {}\n\
                 fn generic<T: Base>() { T::new(); }\n",
            ),
            ("a/src/model.rs", "pub struct X;\n"),
        ]);
        assert_eq!(
            found,
            [
                "a::Derived Uses a::Base",
                "a::caller Calls a::X::run",
                "a::caller Uses a::model::X",
                "a::generic Uses a::Base",
                "a::limit Uses a::LIMIT",
                "a::make Uses a::Store",
                "a::make Calls a::Store::new",
            ]
        );
    }

    #[test]
    fn tests_resolve_from_their_own_module() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            ("a/src/lib.rs", "mod store;\npub fn keep() {}\n"),
            (
                "a/src/store.rs",
                "pub fn keep() {}\npub fn other() {}\n#[test]\nfn alone() { keep(); }\n#[cfg(test)]\nmod tests {\n use super::*;\n\
                 use super::other as kept;\n fn helper() {}\n #[test]\n fn named() { super::keep(); }\n\
                 #[tokio::test(flavor = \"current_thread\")]\n async fn bare() { other(); }\n\
                 mod inner { use super::super::keep as kept;\n #[test]\n fn deep() { kept(); } }\n}\n",
            ),
        ]);
        assert_eq!(
            found,
            [
                "a::store::alone Tests a::store::keep",
                "a::store::tests::bare Tests a::store::other",
                "a::store::tests::inner::deep Tests a::store::keep",
                "a::store::tests::named Tests a::store::keep",
            ]
        );
    }

    #[test]
    fn qualified_types_keep_their_module() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "mod model;\nuse model::X;\npub struct S;\npub trait Tr {}\nimpl Tr for model::S {}\n\
                 impl X { pub fn run() {} }\nfn call() { model::X::run(); }\n",
            ),
            ("a/src/model.rs", "pub struct X;\npub struct S;\n"),
        ]);
        assert_eq!(
            found,
            ["a::call Calls a::X::run", "a::model::S Implements a::Tr"]
        );
    }

    #[test]
    fn a_nested_test_module_reaches_its_parents_uses_through_a_glob() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "pub fn run() {}\nmod m;\n#[cfg(test)]\nmod tests {\n use crate::run;\n use super::m as here;\n\
                 mod inner { use super::*;\n #[test]\n fn checks() { run(); here::go(); } }\n\
                 mod apart { use crate::m::*;\n #[test]\n fn alone() { run(); } }\n}\n",
            ),
            ("a/src/m.rs", "pub fn go() {}\n"),
        ]);
        assert_eq!(
            found,
            [
                "a::tests::inner::checks Tests a::m::go",
                "a::tests::inner::checks Tests a::run",
            ]
        );
    }

    #[test]
    fn a_use_in_a_block_stays_in_it() {
        let found = edges(&[
            ("a/Cargo.toml", CRATE_A),
            (
                "a/src/lib.rs",
                "mod left { pub fn run() {} }\nmod right { pub fn run() {} }\n\
                 fn first() { use crate::left::run; run(); }\nfn second() { run(); }\n",
            ),
        ]);
        assert_eq!(found, ["a::first Calls a::left::run"]);
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
