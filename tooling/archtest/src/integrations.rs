//! Checks of the integrations folder: it holds no code, and only one package reads it.

use std::fs;
use std::path::Path;

use crate::SourceFile;
use crate::rules::{CODE_EXTENSIONS, INTEGRATIONS};

/// Every file under the integrations folder at `root` whose extension marks it as code.
///
/// # Errors
/// When the folder can't be read.
pub(crate) fn check_folder(root: &Path) -> Result<Vec<String>, String> {
    let (folder, _) = INTEGRATIONS;
    let mut out = Vec::new();
    let mut pending = vec![root.join(folder)];
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            let entries =
                fs::read_dir(&path).map_err(|error| format!("{}: {error}", path.display()))?;
            for entry in entries {
                pending.push(entry.map_err(|error| error.to_string())?.path());
            }
        } else if is_code(&path) {
            let path = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();
            out.push(format!("{path} is code; {folder}/ holds only data files"));
        }
    }
    out.sort();
    Ok(out)
}

/// Each of `package`'s files that includes a file from the integrations folder, when the
/// package isn't the one that reads it.
pub(crate) fn check_readers(package: &str, sources: &[SourceFile]) -> Vec<String> {
    let (folder, reader) = INTEGRATIONS;
    if package == reader {
        return Vec::new();
    }
    sources
        .iter()
        .filter(|file| {
            let code = file.text.split("#[cfg(test)]\nmod tests").next();
            includes(code.unwrap_or_default(), folder)
        })
        .map(|file| {
            let path = file.path.display();
            format!("{package}: {path} reads {folder}/, which only {reader} reads")
        })
        .collect()
}

fn is_code(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| CODE_EXTENSIONS.contains(&extension))
}

/// Whether an `include!`, `include_str!` or `include_bytes!` in `text` names `folder`.
fn includes(text: &str, folder: &str) -> bool {
    let named = format!("{folder}/");
    text.match_indices("include")
        .filter_map(|(at, _)| text.get(at..))
        .filter(|rest| {
            ["include!(", "include_str!(", "include_bytes!("]
                .iter()
                .any(|start| rest.starts_with(start))
        })
        .any(|rest| {
            rest.split(')')
                .next()
                .is_some_and(|call| call.contains(&named))
        })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn file(path: &str, text: &str) -> SourceFile {
        SourceFile {
            path: PathBuf::from(path),
            text: text.to_owned(),
        }
    }

    #[test]
    fn only_the_reader_may_include_the_folders_files() {
        let reads = file(
            "src/hook.rs",
            "const SKILL: &str = include_str!(\n    \"../../../integrations/claude/skill.md\"\n);\n",
        );
        let names = file(
            "src/note.rs",
            "// integrations/ is read elsewhere\nconst A: &str = include_str!(\"a.txt\");\n",
        );

        assert_eq!(
            check_readers("weacon-server", &[reads, names]),
            ["weacon-server: src/hook.rs reads integrations/, which only weacon-core reads"]
        );
        let bytes = file("src/b.rs", "include_bytes!(\"integrations/x.png\")");
        assert!(check_readers("weacon-core", &[bytes]).is_empty());
        let tested = file(
            "src/t.rs",
            "#[cfg(test)]\nmod tests { include_str!(\"integrations/a\"); }",
        );
        assert!(check_readers("weacon-agents", &[tested]).is_empty());
        let bytes = file("src/b.rs", "include_bytes!(\"integrations/x.png\")");
        assert_eq!(check_readers("weacon-agents", &[bytes]).len(), 1);
        let code = file("src/c.rs", "include!(\"../integrations/gen.rs\");");
        assert_eq!(check_readers("weacon-agents", &[code]).len(), 1);
    }

    #[test]
    fn the_folder_holds_no_code_and_may_be_missing() {
        let root =
            std::env::temp_dir().join(format!("archtest-integrations-{}", std::process::id()));
        let folder = root.join("integrations/claude/hooks");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("hooks.json"), "{}").unwrap();
        fs::write(folder.join("run.sh"), "echo").unwrap();
        fs::write(root.join("integrations/claude/notes"), "").unwrap();

        let found = check_folder(&root).unwrap();
        let missing = check_folder(&root.join("nowhere")).unwrap();
        fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            found,
            ["integrations/claude/hooks/run.sh is code; integrations/ holds only data files"]
        );
        assert!(missing.is_empty());
    }
}
