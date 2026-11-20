//! Which file uses which. An import is found as a full name, then as a path beside the file
//! that imports, then as the end of a path; a mention is found among what the file imports,
//! then in its own scope, then, as a guess, where one file alone defines the name.

use std::collections::{BTreeSet, HashMap};

use crate::language;

/// Words that stand for a folder above the importing file in an import: `crate::db`, `@/lib/api`.
const ABOVE: [&str; 3] = ["crate", "@", "~"];
/// Words that stand for a folder near it.
const NEAR: [&str; 2] = ["self", "super"];

/// A file of a branch, with what was read from it.
pub struct File {
    pub path: String,
    pub scope: Option<String>,
    /// The names it defines.
    pub names: Vec<String>,
    pub imports: Vec<String>,
    /// The names it mentions, each with whether it is written as a member of something else.
    pub mentions: Vec<(String, bool)>,
}

/// A family of languages with a declared scope, its parts joined by slashes, or with a
/// folder, written after a slash so that the two can't be mistaken.
type Scope = (&'static str, String);

/// What an import leads to: one file, or every file of a scope or a folder.
enum Found {
    File(usize),
    All(Vec<usize>),
}

struct Index<'a> {
    files: &'a [File],
    /// Each file's family of languages, empty for a file we don't read.
    families: Vec<&'static str>,
    /// Each file's folder.
    folders: Vec<&'a str>,
    /// The files that define a full name, a scope and a name joined by slashes.
    named: HashMap<String, Vec<usize>>,
    /// The files of a declared scope, or else of a folder, by family.
    scopes: HashMap<Scope, Vec<usize>>,
    /// Each file's own scope.
    own: Vec<Scope>,
    /// The files and folders an import's path may mean, by their last part. A file is there
    /// under its path, under its path without its extension, and under its folder when it
    /// stands for it; `exact` says the first of these.
    paths: HashMap<&'a str, Vec<Place<'a>>>,
    /// The files that define a name.
    definers: HashMap<&'a str, Vec<usize>>,
}

struct Place<'a> {
    path: &'a str,
    /// A file, or `None` for a folder.
    file: Option<usize>,
    exact: bool,
}

/// An import's parts, with slashes for whatever separates them in its language.
fn parts(text: &str, separator: &str) -> Vec<String> {
    let mut text = text;
    let mut upward = Vec::new();
    // Python counts folders upward in dots: `.price` is beside the file, `..price` one above.
    let dots = text.len() - text.trim_start_matches('.').len();
    if separator == "." && dots > 0 {
        upward = [&["."], &[".."].repeat(dots - 1)[..]].concat();
        text = &text[dots..];
    }
    let parts = text.split(separator).flat_map(|part| part.split('/'));
    let parts = parts.filter(|part| !part.is_empty());
    upward.into_iter().chain(parts).map(str::to_owned).collect()
}

/// A path with its `.` and `..` parts worked out.
fn tidy(folder: &str, parts: &[String]) -> String {
    let mut path: Vec<&str> = folder.split('/').filter(|part| !part.is_empty()).collect();
    for part in parts {
        match part.as_str() {
            "." => {}
            ".." => {
                path.pop();
            }
            part => path.push(part),
        }
    }
    path.join("/")
}

impl<'a> Index<'a> {
    fn new(files: &'a [File]) -> Self {
        let mut index = Index {
            files,
            families: Vec::new(),
            folders: Vec::new(),
            named: HashMap::new(),
            scopes: HashMap::new(),
            own: Vec::new(),
            paths: HashMap::new(),
            definers: HashMap::new(),
        };
        let last = |path: &'a str| path.rsplit('/').next().unwrap_or(path);
        let mut places = Vec::new();
        let mut folders = BTreeSet::new();
        for (at, file) in files.iter().enumerate() {
            let language = language(&file.path);
            let family = language.map_or("", |language| language.family);
            let folder = file.path.rsplit_once('/').map_or("", |(folder, _)| folder);
            index.families.push(family);
            index.folders.push(folder);
            places.push((&*file.path, Some(at), true));
            let mut above = folder;
            while !above.is_empty() && folders.insert(above) {
                above = above.rsplit_once('/').map_or("", |(above, _)| above);
            }

            let separator = language.map_or("/", |language| language.separator);
            let scope = file.scope.as_deref();
            let scope = scope.map(|scope| parts(scope, separator).join("/"));
            for name in &file.names {
                index.definers.entry(name).or_default().push(at);
                let name = parts(name, separator).join("/");
                let full = scope.iter().chain([&name]).cloned().collect::<Vec<_>>();
                index.named.entry(full.join("/")).or_default().push(at);
            }
            let own = (family, scope.unwrap_or_else(|| format!("/{folder}")));
            index.scopes.entry(own.clone()).or_default().push(at);
            index.own.push(own);

            let Some(language) = language else {
                continue;
            };
            let stem = file
                .path
                .rsplit_once('.')
                .map_or(&*file.path, |(stem, _)| stem);
            places.push((stem, Some(at), false));
            if last(stem) == language.index && !folder.is_empty() {
                places.push((folder, Some(at), false));
            }
        }
        places.extend(folders.into_iter().map(|folder| (folder, None, false)));
        for (path, file, exact) in places {
            let place = Place { path, file, exact };
            index.paths.entry(last(path)).or_default().push(place);
        }
        index
    }

    /// Of several files, the one that shares the longest folder path with `from`.
    fn nearest(&self, from: usize, among: impl Iterator<Item = usize>) -> Option<usize> {
        let shared = |file: &usize| {
            let (a, b) = (
                self.folders[from].split('/'),
                self.folders[*file].split('/'),
            );
            a.zip(b).take_while(|(a, b)| a == b).count()
        };
        // Of equals, the first.
        among.fold(None, |best, file| match best {
            Some(best) if shared(&best) >= shared(&file) => Some(best),
            _ => Some(file),
        })
    }

    /// The file or folder that a path means to the file `from`. With `ending`, the path may be
    /// the end of a longer one; with `above`, the highest such file over `from` is meant.
    fn place(&self, from: usize, path: &str, ending: bool, above: bool) -> Option<Found> {
        let last = path.rsplit('/').next().unwrap_or(path);
        let fits = |place: &&Place| {
            let whole = place.path == path;
            let ends = ending
                && place.path.ends_with(path)
                && place.path[..place.path.len() - path.len()].ends_with('/');
            // Only a path written with its extension reaches a file of another language.
            let reachable = place
                .file
                .is_none_or(|file| place.exact || self.families[file] == self.families[from]);
            (whole || ends) && reachable
        };
        let places: Vec<&Place> = (self.paths.get(last).into_iter().flatten())
            .filter(fits)
            .collect();
        // What stands before the path in a place's own, when that is a folder over `from`.
        let over = |place: &&&Place| {
            let before = &place.path[..place.path.len() - path.len()];
            format!("{}/", self.folders[from]).starts_with(before)
        };
        let highest = places.iter().filter(|place| above && place.file.is_some());
        let highest = highest.filter(over).min_by_key(|place| place.path.len());
        let files = places.iter().filter_map(|place| place.file);
        let file = highest.and_then(|place| place.file);
        if let Some(file) = file.or_else(|| self.nearest(from, files)) {
            return Some(Found::File(file));
        }
        // A folder stands for the files in it, as a package does in Go.
        if let Some(folder) = places.first() {
            let inside = self
                .scopes
                .get(&(self.families[from], format!("/{}", folder.path)))?;
            return Some(Found::All(inside.clone()));
        }
        // TypeScript names the file it will become: `./price.js` is `price.ts`.
        let (stem, _) = path.rsplit_once('.')?;
        let same = language(path).is_some_and(|language| language.family == self.families[from]);
        (same && !stem.ends_with('/')).then(|| self.place(from, stem, ending, above))?
    }

    /// What an import of the file `from` leads to.
    fn find(&self, from: usize, import: &str) -> Option<Found> {
        let language = language(&self.files[from].path)?;
        let parts = parts(import, language.separator);
        // Where a dot or the like separates the parts, the last may be a name inside what is
        // imported: `crate::db::Doc`, `a.b.C.method`.
        let shorter =
            (language.separator != "/" && parts.len() > 1).then(|| &parts[..parts.len() - 1]);
        [&parts[..]].into_iter().chain(shorter).find_map(|parts| {
            let name = parts.join("/");
            // A file that defines the very name it imports only says where it is, as `mod db;`
            // does in Rust, so the name is then looked for as a path.
            let declared = self.files[from].names.contains(&name);
            let named = self.named.get(&name).into_iter().flatten();
            let named: Vec<usize> = named.copied().filter(|_| !declared).collect();
            // Of two files that define it, the one whose path follows the name further is meant:
            // `App\Models\User` is `app/Models/User.php` before it is `Models/User.php`.
            let follows = |file: &usize| {
                let path = self.files[*file].path.to_lowercase();
                let path = path.rsplit_once('.').map_or(&*path, |(stem, _)| stem);
                let (path, name) = (path.rsplit('/'), parts.iter().rev());
                path.zip(name)
                    .take_while(|(a, b)| **a == b.to_lowercase())
                    .count()
            };
            let most = named.iter().map(follows).max();
            let named = named
                .iter()
                .copied()
                .filter(|file| Some(follows(file)) == most);
            if let Some(file) = self.nearest(from, named) {
                return Some(Found::File(file));
            }
            if let Some(files) = self.scopes.get(&(self.families[from], name.clone())) {
                return Some(Found::All(files.clone()));
            }
            let beside = tidy(self.folders[from], parts);
            if let Some(found) = self.place(from, &beside, false, false) {
                return Some(found);
            }
            if parts
                .first()
                .is_some_and(|part| part == "." || part == "..")
            {
                return None;
            }
            if let Some(found) = self.place(from, &name, false, false) {
                return Some(found);
            }
            // One part alone could be anything, unless a word for a folder came before it.
            // Dart names a package the same way: `package:shop/price.dart`.
            let above = parts.first().is_some_and(|part| ABOVE.contains(&&**part));
            let root = |part: &&String| {
                ABOVE.contains(&&***part) || NEAR.contains(&&***part) || part.contains(':')
            };
            let rooted = parts.iter().take_while(root).count();
            let least = if rooted > 0 { 1 } else { 2 };
            (rooted..parts.len())
                .take_while(|start| parts.len() - start >= least)
                .find_map(|start| self.place(from, &parts[start..].join("/"), true, above))
        })
    }
}

/// The pairs of a file and a file it uses, each as its place in `files`.
pub fn link(files: &[File]) -> Vec<(usize, usize)> {
    let index = Index::new(files);
    let mut uses = BTreeSet::new();
    for (at, file) in files.iter().enumerate() {
        let family = index.families[at];
        if family.is_empty() {
            continue;
        }
        // What the file can reach through its imports.
        let mut reach = BTreeSet::new();
        // The names it imports from outside the repository, which no file in it is then guessed to define.
        let mut foreign = BTreeSet::new();
        let separator = language(&file.path).map_or("/", |language| language.separator);
        for import in &file.imports {
            match index.find(at, import) {
                Some(Found::File(found)) => {
                    reach.insert(found);
                    uses.insert((at, found));
                }
                Some(Found::All(found)) => reach.extend(found),
                None => {
                    foreign.extend(import.rsplit(separator).next());
                }
            }
        }
        let own = index.scopes.get(&index.own[at]);
        for (name, member) in &file.mentions {
            let definers = index.definers.get(name.as_str()).into_iter().flatten();
            let definers: Vec<usize> = definers
                .copied()
                .filter(|other| *other != at && index.families[*other] == family)
                .collect();
            // Of several files that define the name, the nearest is meant.
            let nearest = |among: &dyn Fn(&usize) -> bool| {
                index.nearest(at, definers.iter().copied().filter(among))
            };
            let found = nearest(&|file| reach.contains(file)).or_else(|| {
                // A member's name says too little to look further than the imports.
                if *member {
                    return None;
                }
                let near = nearest(&|file| own.is_some_and(|own| own.contains(file)));
                // The one guess: a name one file alone defines, when it starts with a capital,
                // as the names of types and components do and those of passing calls don't.
                let guessed = name.starts_with(char::is_uppercase)
                    && definers.len() == 1
                    && !foreign.contains(name.as_str());
                near.or_else(|| guessed.then(|| definers[0]))
            });
            uses.extend(found.map(|found| (at, found)));
        }
    }
    uses.into_iter()
        .filter(|(user, used)| user != used)
        .collect()
}
