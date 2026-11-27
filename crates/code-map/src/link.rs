use std::collections::{BTreeSet, HashMap};

use crate::language;

const ABOVE: [&str; 3] = ["crate", "@", "~"];
const NEAR: [&str; 2] = ["self", "super"];

pub struct File {
    pub path: String,
    pub scope: Option<String>,
    pub names: Vec<String>,
    pub imports: Vec<String>,
    pub mentions: Vec<(String, bool)>,
}

type Scope = (&'static str, String);

enum Found {
    File(usize),
    All(Vec<usize>),
}

struct Index<'a> {
    files: &'a [File],
    families: Vec<&'static str>,
    folders: Vec<&'a str>,
    named: HashMap<String, Vec<usize>>,
    scopes: HashMap<Scope, Vec<usize>>,
    own: Vec<Scope>,
    paths: HashMap<&'a str, Vec<Place<'a>>>,
    definers: HashMap<&'a str, Vec<usize>>,
}

struct Place<'a> {
    path: &'a str,
    file: Option<usize>,
    exact: bool,
}

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
            let scope = file.scope.as_deref().map(|scope| parts(scope, separator).join("/"));
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
            let stem = file.path.rsplit_once('.').map_or(&*file.path, |(stem, _)| stem);
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

    fn nearest(&self, from: usize, among: impl Iterator<Item = usize>) -> Option<usize> {
        let shared = |file: &usize| {
            let (a, b) = (self.folders[from].split('/'), self.folders[*file].split('/'));
            a.zip(b).take_while(|(a, b)| a == b).count()
        };
        among.fold(None, |best, file| match best {
            Some(best) if shared(&best) >= shared(&file) => Some(best),
            _ => Some(file),
        })
    }

    fn place(&self, from: usize, path: &str, ending: bool, above: bool) -> Option<Found> {
        let last = path.rsplit('/').next().unwrap_or(path);
        let fits = |place: &&Place| {
            let whole = place.path == path;
            let ends = ending
                && place.path.ends_with(path)
                && place.path[..place.path.len() - path.len()].ends_with('/');
            let reachable =
                place.file.is_none_or(|file| place.exact || self.families[file] == self.families[from]);
            (whole || ends) && reachable
        };
        let places: Vec<&Place> = (self.paths.get(last).into_iter().flatten()).filter(fits).collect();
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
        if let Some(folder) = places.first() {
            let inside = self.scopes.get(&(self.families[from], format!("/{}", folder.path)))?;
            return Some(Found::All(inside.clone()));
        }
        // TypeScript names the file it will become: `./price.js` is `price.ts`.
        let (stem, _) = path.rsplit_once('.')?;
        let same = language(path).is_some_and(|language| language.family == self.families[from]);
        (same && !stem.ends_with('/')).then(|| self.place(from, stem, ending, above))?
    }

    fn find(&self, from: usize, import: &str) -> Option<Found> {
        let language = language(&self.files[from].path)?;
        let parts = parts(import, language.separator);
        let shorter = (language.separator != "/" && parts.len() > 1).then(|| &parts[..parts.len() - 1]);
        [&parts[..]].into_iter().chain(shorter).find_map(|parts| {
            let name = parts.join("/");
            let declared = self.files[from].names.contains(&name);
            let named = self.named.get(&name).into_iter().flatten();
            let named: Vec<usize> = named.copied().filter(|_| !declared).collect();
            let follows = |file: &usize| {
                let path = self.files[*file].path.to_lowercase();
                let path = path.rsplit_once('.').map_or(&*path, |(stem, _)| stem);
                let (path, name) = (path.rsplit('/'), parts.iter().rev());
                path.zip(name).take_while(|(a, b)| **a == b.to_lowercase()).count()
            };
            let most = named.iter().map(follows).max();
            let named = named.iter().copied().filter(|file| Some(follows(file)) == most);
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
            if parts.first().is_some_and(|part| part == "." || part == "..") {
                return None;
            }
            if let Some(found) = self.place(from, &name, false, false) {
                return Some(found);
            }
            let above = parts.first().is_some_and(|part| ABOVE.contains(&&**part));
            let root =
                |part: &&String| ABOVE.contains(&&***part) || NEAR.contains(&&***part) || part.contains(':');
            let rooted = parts.iter().take_while(root).count();
            let least = if rooted > 0 { 1 } else { 2 };
            (rooted..parts.len())
                .take_while(|start| parts.len() - start >= least)
                .find_map(|start| self.place(from, &parts[start..].join("/"), true, above))
        })
    }
}

pub fn link(files: &[File]) -> Vec<(usize, usize)> {
    let index = Index::new(files);
    let mut uses = BTreeSet::new();
    for (at, file) in files.iter().enumerate() {
        let family = index.families[at];
        if family.is_empty() {
            continue;
        }
        let mut reach = BTreeSet::new();
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
            let definers: Vec<usize> =
                definers.copied().filter(|other| *other != at && index.families[*other] == family).collect();
            let nearest =
                |among: &dyn Fn(&usize) -> bool| index.nearest(at, definers.iter().copied().filter(among));
            let found = nearest(&|file| reach.contains(file)).or_else(|| {
                if *member {
                    return None;
                }
                let near = nearest(&|file| own.is_some_and(|own| own.contains(file)));
                let guessed = name.starts_with(char::is_uppercase)
                    && definers.len() == 1
                    && !foreign.contains(name.as_str());
                near.or_else(|| guessed.then(|| definers[0]))
            });
            uses.extend(found.map(|found| (at, found)));
        }
    }
    uses.into_iter().filter(|(user, used)| user != used).collect()
}
