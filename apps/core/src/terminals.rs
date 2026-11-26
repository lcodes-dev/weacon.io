use std::io::{Read, Write};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use core_link::{Agent, FromCore};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

use crate::agents;
use crate::link::{Shared, Work};

pub struct Terminal {
    number: u64,
    pty: Box<dyn MasterPty + Send>,
    typed: std::sync::mpsc::Sender<Vec<u8>>,
    sign_in: Box<dyn ChildKiller + Send + Sync>,
}

pub fn open(
    work: &Shared,
    agent: Agent,
    cols: u16,
    rows: u16,
) -> Result<(), Box<dyn std::error::Error>> {
    let pair = native_pty_system().openpty(size(cols, rows))?;
    let mut command = CommandBuilder::new(agent.program());
    command.args(agents::sign_in(agent));
    command.env("TERM", "xterm-256color");
    let mut child = pair.slave.spawn_command(command)?;
    drop(pair.slave);
    let mut shown = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
    let (typed, typing) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for typed in typing {
            if writer.write_all(&typed).is_err() {
                break;
            }
        }
    });
    let number = {
        let mut work = work.lock().unwrap();
        work.opened += 1;
        let terminal = Terminal {
            number: work.opened,
            typed,
            pty: pair.master,
            sign_in: child.clone_killer(),
        };
        if let Some(mut earlier) = work.terminals.insert(agent, terminal) {
            earlier.sign_in.kill().ok();
        }
        work.opened
    };
    let work = work.clone();
    std::thread::spawn(move || {
        let current = |work: &Work| {
            work.terminals
                .get(&agent)
                .is_some_and(|open| open.number == number)
        };
        let mut bytes = [0; 4096];
        while let Ok(read @ 1..) = shown.read(&mut bytes) {
            let data = BASE64.encode(&bytes[..read]);
            let work = work.lock().unwrap();
            if let (true, Some(out)) = (current(&work), &work.out) {
                out.send(FromCore::TerminalData { agent, data }).ok();
            }
        }
        child.wait().ok();
        let mut work = work.lock().unwrap();
        if current(&work) {
            work.terminals.remove(&agent);
            if let Some(out) = &work.out {
                out.send(FromCore::TerminalClose { agent }).ok();
            }
        }
        work.changed.notify_one();
    });
    Ok(())
}

pub fn typed(work: &Shared, agent: Agent, data: &str) {
    let typed = BASE64.decode(data).unwrap_or_default();
    if let Some(terminal) = work.lock().unwrap().terminals.get(&agent) {
        terminal.typed.send(typed).ok();
    }
}

pub fn resize(work: &Shared, agent: Agent, cols: u16, rows: u16) {
    if let Some(terminal) = work.lock().unwrap().terminals.get(&agent) {
        terminal.pty.resize(size(cols, rows)).ok();
    }
}

pub fn close(work: &Shared, agent: Agent) {
    if let Some(mut terminal) = work.lock().unwrap().terminals.remove(&agent) {
        terminal.sign_in.kill().ok();
    }
}

pub fn close_all(work: &mut Work) {
    for (_, mut terminal) in work.terminals.drain() {
        terminal.sign_in.kill().ok();
    }
}

fn size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    }
}
