//! A chat's folder, opened in its owner's own terminal on this machine when they ask from the
//! web app.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use weacon_observability::Fault;
use weacon_protocol::{WorktreeAnswerKind, code};

use crate::platform::{self, Env};
use crate::runs::ChatKey;

/// Opens the person's terminal in a folder, with the core's own arguments after its program;
/// false where this machine has none to open. Tests pass one of their own.
pub(crate) type OpenTerminal = fn(&Path, &[OsString]) -> io::Result<bool>;

/// Whether a terminal opens here at all; any absolute folder tells, so the state folder does.
pub(crate) fn offered(state_dir: &Path, env: Env<'_>) -> bool {
    platform::terminal_in(state_dir, env, &[]).is_some()
}

/// Opens the owner's terminal in the chat's folder, where it resumes the chat if the terminal
/// takes a command. Only the two ids vary on its command line, each after its option's `=`.
pub(crate) fn open_in_chat(
    work_root: Option<&Path>,
    key: &ChatKey,
    open: OpenTerminal,
) -> Result<WorktreeAnswerKind, Fault> {
    let none = || Fault::new(code!("terminal.no_folder"), "The chat has no folder here");
    let folder = crate::chats::chat_folder(work_root.ok_or_else(none)?, &key.workspace, &key.chat);
    if !folder.is_dir() {
        return Err(none());
    }
    let resume = [
        OsString::from("resume"),
        format!("--workspace={}", key.workspace.as_str()).into(),
        format!("--chat={}", key.chat.as_str()).into(),
    ];
    match open(&folder, &resume) {
        Ok(true) => Ok(WorktreeAnswerKind::TerminalOpened),
        Ok(false) => Err(Fault::new(
            code!("terminal.none"),
            "This machine has no desktop terminal",
        )),
        Err(error) => {
            Err(Fault::new(code!("terminal.failed"), "The terminal couldn't open").because(error))
        }
    }
}
