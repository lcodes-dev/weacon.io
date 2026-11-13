use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The web app's files the server serves; `npm --prefix apps/web run build` makes them.
const WEB_FILES: [&str; 3] = ["index.html", "app.js", "app.css"];

fn main() -> io::Result<()> {
    weacon_version::emit();
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../web/dist");
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap_or_default());
    fs::write(out.join("web.rs"), embed(&dist)?)
}

/// A Rust list of the built files that are there, each a name and its bytes; an empty one
/// when the web app wasn't built, as in a Rust-only check.
fn embed(dist: &Path) -> io::Result<String> {
    // Watching the folder too notices a file that appears or goes.
    let watched = if dist.is_dir() {
        dist
    } else {
        dist.parent().unwrap_or(dist)
    };
    println!("cargo::rerun-if-changed={}", watched.display());
    let mut list = String::from("&[");
    for name in WEB_FILES {
        let path = dist.join(name);
        if path.is_file() {
            println!("cargo::rerun-if-changed={}", path.display());
            let path = path.display().to_string();
            write!(list, "({name:?}, include_bytes!({path:?})),").map_err(io::Error::other)?;
        }
    }
    Ok(list + "]")
}
