#![expect(
    unsafe_code,
    reason = "std has no pseudo-consoles: ConPTY and starting a process in one are only in the Windows API"
)]

mod command;

use std::ffi::c_void;
use std::io::{self, PipeReader, PipeWriter, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};

use tokio::process::Command;
use tokio::sync::{mpsc, watch};
use tokio::task;
use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::System::Console::{
    COORD, ClosePseudoConsole, CreatePseudoConsole, HPCON, ResizePseudoConsole,
};
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, STARTUPINFOW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};

use super::CHUNK;
use crate::terminal::{Event, Size};
use command::{command_line, environment, wide};

/// The program's side ends when it does; dropping this ends the program.
pub(crate) struct Pty {
    input: PipeWriter,
    console: Arc<Console>,
    process: Arc<OwnedHandle>,
    // Dropped with this, which tells the output's reader to stop waiting for the events' reader.
    _stop: watch::Sender<()>,
}

// The pseudo-console, until it is closed once the program has ended.
struct Console(Mutex<Option<HPCON>>);

impl Pty {
    /// Starts `command`, which must name a program file and not a script, in a new
    /// pseudo-console of `size`.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the same signature as on Unix, where starting consumes the command"
    )]
    pub(crate) fn spawn(command: Command, size: Size) -> io::Result<(Self, mpsc::Receiver<Event>)> {
        let command = command.as_std();
        let program = Path::new(command.get_program());
        if !program
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("exe"))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a program file",
            ));
        }
        let (input_read, input) = io::pipe()?;
        let (output, output_write) = io::pipe()?;
        let mut handle = 0;
        // SAFETY: both pipe ends are open; the console duplicates them.
        let created = unsafe {
            CreatePseudoConsole(
                coord(size),
                input_read.as_raw_handle(),
                output_write.as_raw_handle(),
                0,
                &raw mut handle,
            )
        };
        if created < 0 {
            return Err(failed(created));
        }
        drop((input_read, output_write));
        let console = Arc::new(Console(Mutex::new(Some(handle))));
        let process = start(command, program, handle).inspect_err(|_| console.close())?;
        let process = Arc::new(process);
        let (events, received) = mpsc::channel(8);
        let (stop, stopped) = watch::channel(());
        let watched = (Arc::clone(&process), Arc::clone(&console));
        tokio::spawn(supervise(watched, output, events, stopped));
        let pty = Self {
            input,
            console,
            process,
            _stop: stop,
        };
        Ok((pty, received))
    }

    /// Writes all of `data` as if typed.
    pub(crate) async fn write(&self, data: &[u8]) -> io::Result<()> {
        let mut input = self.input.try_clone()?;
        let data = data.to_vec();
        task::spawn_blocking(move || input.write_all(&data))
            .await
            .map_err(io::Error::other)?
    }

    pub(crate) fn resize(&self, size: Size) -> io::Result<()> {
        let open = self
            .console
            .0
            .lock()
            .map_err(|_| io::ErrorKind::BrokenPipe)?;
        let Some(handle) = *open else {
            return Err(io::ErrorKind::BrokenPipe.into());
        };
        // SAFETY: the lock is held, so the console can't close during the call.
        let resized = unsafe { ResizePseudoConsole(handle, coord(size)) };
        if resized < 0 {
            return Err(failed(resized));
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        // SAFETY: the process handle is open; ending a program that already ended does nothing.
        unsafe { TerminateProcess(self.process.as_raw_handle(), 1) };
        // Not left to `supervise`, which a runtime that is stopping no longer runs: until the
        // console closes, the thread reading its output can't end, nor can the runtime.
        let console = Arc::clone(&self.console);
        let closing = std::thread::Builder::new().spawn(move || console.close());
        if closing.is_err() {
            self.console.close();
        }
    }
}

impl Console {
    // Closing waits until the output is read, which `pump` keeps doing.
    fn close(&self) {
        let taken = self.0.lock().ok().and_then(|mut handle| handle.take());
        if let Some(handle) = taken {
            // SAFETY: the handle was open and is closed only here, once.
            unsafe { ClosePseudoConsole(handle) };
        }
    }
}

// Sends the output and then how the program ended. The output ends only when the console
// closes, which happens once the program has ended.
async fn supervise(
    (process, console): (Arc<OwnedHandle>, Arc<Console>),
    output: PipeReader,
    events: mpsc::Sender<Event>,
    stopped: watch::Receiver<()>,
) {
    let sender = events.clone();
    let pump = task::spawn_blocking(move || pump(output, &sender, stopped));
    let success = task::spawn_blocking(move || ended_well(&process))
        .await
        .unwrap_or(false);
    drop(task::spawn_blocking(move || console.close()).await);
    drop(pump.await);
    drop(events.send(Event::Exited { success }).await);
}

// Keeps reading once nobody listens, or once the terminal is dropped, so the console can still
// close.
fn pump(mut output: PipeReader, events: &mpsc::Sender<Event>, mut stopped: watch::Receiver<()>) {
    let runtime = tokio::runtime::Handle::current();
    let mut buffer = vec![0; CHUNK];
    let mut listening = true;
    while let Ok(read) = output.read(&mut buffer) {
        if read == 0 {
            return;
        }
        let chunk = buffer.get(..read).unwrap_or_default().to_vec();
        listening = listening
            && runtime.block_on(async {
                tokio::select! {
                    sent = events.send(Event::Output(chunk)) => sent.is_ok(),
                    _ = stopped.changed() => false,
                }
            });
    }
}

fn ended_well(process: &OwnedHandle) -> bool {
    let mut code = 1;
    // SAFETY: the handle is open for both calls, and `code` is room for the exit code.
    unsafe {
        WaitForSingleObject(process.as_raw_handle(), INFINITE) == WAIT_OBJECT_0
            && GetExitCodeProcess(process.as_raw_handle(), &raw mut code) != 0
            && code == 0
    }
}

// Starts the program attached to `console`, with the command's arguments, environment and
// folder, and no handles of this process.
fn start(
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

// An HRESULT is no OS error code, so it travels as a number only.
fn failed(result: i32) -> io::Error {
    io::Error::other(format!("HRESULT {result:#010x}"))
}

fn check(succeeded: i32) -> io::Result<()> {
    if succeeded == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn coord(size: Size) -> COORD {
    COORD {
        X: i16::try_from(size.cols).unwrap_or(i16::MAX),
        Y: i16::try_from(size.rows).unwrap_or(i16::MAX),
    }
}
