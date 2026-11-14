use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The web app's files the server serves, each with its route and type;
/// `npm --prefix apps/web run build` makes them.
const WEB_FILES: [(&str, &str, &str); 3] = [
    ("index.html", "/", "text/html; charset=utf-8"),
    ("app.js", "/app.js", "text/javascript; charset=utf-8"),
    ("app.css", "/app.css", "text/css; charset=utf-8"),
];

fn main() -> io::Result<()> {
    weacon_version::emit();
    // Read as the script runs: `env!` would keep the folder of the checkout it was built in.
    let var = |name| PathBuf::from(env::var_os(name).unwrap_or_default());
    let dist = var("CARGO_MANIFEST_DIR").join("../web/dist");
    let out = var("OUT_DIR").join("web.rs");
    let list = embed(&dist)?;
    // Rewriting the same list would rebuild the server for nothing.
    if fs::read_to_string(&out).ok().as_deref() != Some(list.as_str()) {
        fs::write(out, list)?;
    }
    Ok(())
}

/// A Rust list of the built files, each its route, type and bytes: all of them, or none when
/// the web app wasn't built, as in a Rust-only check.
fn embed(dist: &Path) -> io::Result<String> {
    println!("cargo::rerun-if-changed={}", dist.display());
    let paths: Vec<_> = WEB_FILES.iter().map(|(name, ..)| dist.join(name)).collect();
    if !paths.iter().all(|path| path.is_file()) {
        return Ok("&[]".to_owned());
    }
    let mut list = String::from("&[");
    for ((_, route, kind), path) in WEB_FILES.iter().zip(paths) {
        let path = path.display().to_string();
        write!(list, "({route:?}, {kind:?}, include_bytes!({path:?})),")
            .map_err(io::Error::other)?;
    }
    Ok(list + "]")
}
