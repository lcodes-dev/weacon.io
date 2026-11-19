//! Prints which file uses which in a folder on this computer: `cargo run -p code-map --example map -- ../some/project`.
//! With a second argument, only the lines to and from paths that hold it.
use std::path::Path;

use code_map::{File, link, read};

/// Folders too large to be worth walking.
const SKIPPED: [&str; 4] = [".git", "node_modules", "target", "vendor"];

fn walk(root: &Path, folder: &Path, files: &mut Vec<File>) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if !SKIPPED.contains(&&*name.to_string_lossy()) {
                walk(root, &path, files);
            }
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let path = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let reading = read(&path, &bytes);
        files.push(File {
            path,
            scope: reading.scope,
            names: reading
                .symbols
                .into_iter()
                .map(|symbol| symbol.name)
                .collect(),
            imports: reading.imports.into_iter().collect(),
            mentions: reading.mentions.into_iter().collect(),
        });
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().expect("a folder to map");
    let only = args.next().unwrap_or_default();
    let mut files = Vec::new();
    walk(Path::new(&root), Path::new(&root), &mut files);
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let uses = link(&files);
    for (user, used) in &uses {
        let (user, used) = (&files[*user].path, &files[*used].path);
        if user.contains(&only) || used.contains(&only) {
            println!("{user} -> {used}");
        }
    }
    eprintln!("{} files, {} lines", files.len(), uses.len());
}
