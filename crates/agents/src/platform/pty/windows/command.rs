//! The command line and environment block `CreateProcessW` takes, built from a command.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::iter;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

// `text` as UTF-16; text that holds a NUL itself is refused.
fn units(text: &OsStr) -> io::Result<Vec<u16>> {
    let units: Vec<u16> = text.encode_wide().collect();
    if units.contains(&0) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    Ok(units)
}

// `text` as UTF-16 ending in NUL.
pub(super) fn wide(text: &OsStr) -> io::Result<Vec<u16>> {
    let mut wide = units(text)?;
    wide.push(0);
    Ok(wide)
}

const SPACE: u16 = 0x20;
const TAB: u16 = 0x09;
const QUOTE: u16 = 0x22;
const BACKSLASH: u16 = 0x5c;

// The program and its arguments as one command line, each quoted so the program's own parser
// gets them back exactly.
pub(super) fn command_line<'a>(
    program: &Path,
    args: impl Iterator<Item = &'a OsStr>,
) -> io::Result<Vec<u16>> {
    let mut line = Vec::new();
    push_quoted(&mut line, program.as_os_str())?;
    for arg in args {
        line.push(SPACE);
        push_quoted(&mut line, arg)?;
    }
    line.push(0);
    Ok(line)
}

fn push_quoted(line: &mut Vec<u16>, arg: &OsStr) -> io::Result<()> {
    let units = units(arg)?;
    if !units.is_empty() && !units.iter().any(|unit| [SPACE, TAB, QUOTE].contains(unit)) {
        line.extend(units);
        return Ok(());
    }
    line.push(QUOTE);
    let mut backslashes = 0;
    for unit in units {
        if unit == BACKSLASH {
            backslashes += 1;
            continue;
        }
        // Backslashes count as such only before a quote, where each must be doubled.
        let escaped = if unit == QUOTE {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        line.extend(iter::repeat_n(BACKSLASH, escaped));
        line.push(unit);
        backslashes = 0;
    }
    line.extend(iter::repeat_n(BACKSLASH, backslashes * 2));
    line.push(QUOTE);
    Ok(())
}

// This process's environment with the command's changes, as the block `CreateProcessW` takes:
// sorted by name in any case, and ending in two NULs.
pub(super) fn environment(command: &std::process::Command) -> io::Result<Vec<u16>> {
    let mut variables: BTreeMap<OsString, (OsString, OsString)> = std::env::vars_os()
        .map(|(name, value)| (name.to_ascii_uppercase(), (name, value)))
        .collect();
    for (name, value) in command.get_envs() {
        let key = name.to_ascii_uppercase();
        match value {
            Some(value) => variables.insert(key, (name.to_owned(), value.to_owned())),
            None => variables.remove(&key),
        };
    }
    let mut block = Vec::new();
    for (name, value) in variables.values() {
        let mut entry = name.clone();
        entry.push("=");
        entry.push(value);
        block.extend(wide(&entry)?);
    }
    block.push(0);
    if block.len() == 1 {
        block.push(0);
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(program: &str, args: &[&str]) -> String {
        let line = command_line(Path::new(program), args.iter().map(OsStr::new)).unwrap();
        String::from_utf16(line.strip_suffix(&[0]).unwrap()).unwrap()
    }

    #[test]
    fn arguments_are_quoted_only_as_they_need() {
        assert_eq!(
            line(r"C:\bin\claude.exe", &["auth", "login"]),
            r"C:\bin\claude.exe auth login"
        );
        assert_eq!(
            line(
                r"C:\Program Files\claude.exe",
                &["", "a b", r#"say "hi""#, r"dir\", r"a\\b"]
            ),
            r#""C:\Program Files\claude.exe" "" "a b" "say \"hi\"" dir\ a\\b"#
        );
        assert_eq!(
            line("x.exe", &[r#"\""#, "end\\ x"]),
            r#"x.exe "\\\"" "end\ x""#
        );
        command_line(Path::new("x.exe"), iter::once(OsStr::new("a\0b"))).unwrap_err();
    }

    #[test]
    fn the_environment_block_takes_the_commands_changes_in_any_case() {
        let mut command = std::process::Command::new("x.exe");
        command
            .env("Weacon_Test_Set", "1")
            .env_remove("PATH")
            .env_remove("pathext");
        let block = String::from_utf16(&environment(&command).unwrap()).unwrap();
        let entries: Vec<&str> = block.trim_end_matches('\0').split('\0').collect();
        // The entries hold this process's whole environment, so no message shows them.
        assert!(entries.contains(&"Weacon_Test_Set=1"));
        assert!(
            !entries
                .iter()
                .any(|entry| entry.to_ascii_uppercase().starts_with("PATH="))
        );
        assert!(
            !entries
                .iter()
                .any(|entry| entry.to_ascii_uppercase().starts_with("PATHEXT="))
        );
        assert!(
            entries.is_sorted_by_key(|entry| entry.split('=').next().map(str::to_ascii_uppercase))
        );
        assert!(block.ends_with("\0\0"));
    }
}
