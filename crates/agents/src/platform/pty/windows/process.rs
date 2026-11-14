//! Starting a program attached to a pseudo-console.
#![expect(
    unsafe_code,
    reason = "std can't start a process attached to a pseudo-console: only the Windows API can"
)]

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;

use windows_sys::Win32::System::Console::HPCON;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOEXW,
    STARTUPINFOW, UpdateProcThreadAttribute,
};

use super::command::{command_line, environment, wide};

/// Whether `program` names a program file, which Windows starts as it is, and so no script.
pub(super) fn is_program_file(program: &Path) -> bool {
    program
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
}

// Starts the program attached to `console`, with the command's arguments, environment and
// folder, and no handles of this process.
pub(super) fn start(
    command: &std::process::Command,
    program: &Path,
    console: HPCON,
) -> io::Result<OwnedHandle> {
    let mut attributes = Attributes::new(console)?;
    let startup = STARTUPINFOEXW {
        StartupInfo: STARTUPINFOW {
            cb: u32::try_from(size_of::<STARTUPINFOEXW>()).map_err(io::Error::other)?,
            // Null, so the console gives the program its own standard handles rather than it
            // getting this process's.
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: ptr::null_mut(),
            hStdOutput: ptr::null_mut(),
            hStdError: ptr::null_mut(),
            ..STARTUPINFOW::default()
        },
        lpAttributeList: attributes.list(),
    };
    let application = wide(program.as_os_str())?;
    let mut line = command_line(program, command.get_args())?;
    let environment = environment(command)?;
    let folder = command
        .get_current_dir()
        .map(|dir| wide(dir.as_os_str()))
        .transpose()?;
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every string ends in NUL and outlives the call, `line` is writable as the call
    // needs, and the environment block ends in two NULs.
    check(unsafe {
        CreateProcessW(
            application.as_ptr(),
            line.as_mut_ptr(),
            ptr::null(),
            ptr::null(),
            0,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            environment.as_ptr().cast::<c_void>(),
            folder.as_ref().map_or(ptr::null(), Vec::as_ptr),
            &raw const startup.StartupInfo,
            &raw mut info,
        )
    })?;
    // SAFETY: the call succeeded, so both handles are open and this process's to close.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess),
            OwnedHandle::from_raw_handle(info.hThread),
        )
    };
    drop(thread);
    Ok(process)
}

// A list of one attribute, the pseudo-console, in room of its own.
struct Attributes(Vec<u64>);

impl Attributes {
    fn new(console: HPCON) -> io::Result<Self> {
        let mut size = 0;
        // SAFETY: with no list, this only says how large one must be.
        unsafe { InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &raw mut size) };
        let mut attributes = Self(vec![0; size.div_ceil(8)]);
        // SAFETY: the room is at least `size` bytes, aligned for the list.
        check(unsafe {
            InitializeProcThreadAttributeList(attributes.list(), 1, 0, &raw mut size)
        })?;
        let attribute =
            usize::try_from(PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE).map_err(io::Error::other)?;
        // SAFETY: the list has room for one attribute, and the console outlives the start. The
        // attribute's value is the console handle itself.
        check(unsafe {
            UpdateProcThreadAttribute(
                attributes.list(),
                0,
                attribute,
                ptr::without_provenance(console.cast_unsigned()),
                size_of::<HPCON>(),
                ptr::null_mut(),
                ptr::null(),
            )
        })?;
        Ok(attributes)
    }

    fn list(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.0.as_mut_ptr().cast()
    }
}

impl Drop for Attributes {
    fn drop(&mut self) {
        // SAFETY: the list was initialized in this room, which is still here.
        unsafe { DeleteProcThreadAttributeList(self.list()) };
    }
}

fn check(succeeded: i32) -> io::Result<()> {
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_program_file_is_started_never_a_script() {
        for program in [r"C:\bin\claude.exe", r"C:\bin\CLAUDE.EXE", "codex.Exe"] {
            assert!(is_program_file(Path::new(program)), "{program}");
        }
        let refused = [
            r"C:\bin\claude.cmd",
            r"C:\bin\claude.bat",
            r"C:\bin\claude.ps1",
            r"C:\bin\claude",
            r"C:\bin\claude.exe.cmd",
            r"C:\bin\exe",
        ];
        for program in refused {
            assert!(!is_program_file(Path::new(program)), "{program}");
        }
    }
}
