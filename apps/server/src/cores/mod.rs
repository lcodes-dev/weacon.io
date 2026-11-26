mod asks;
mod connection;
pub mod terminal;

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use core_link::{Agent, FromCore, ToCore};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

pub use asks::{answered, ask};
pub use connection::serve;

use crate::error::{ApiError, Result};

type Answer = std::result::Result<String, String>;

#[derive(Default)]
pub struct Cores(Mutex<Connected>);

#[derive(Default)]
struct Connected {
    cores: HashMap<String, (u64, mpsc::UnboundedSender<ToCore>)>,
    left: HashMap<String, Instant>,
    asks: HashMap<String, (String, ToCore, oneshot::Sender<Answer>)>,
    terminals: HashMap<(String, Agent), mpsc::UnboundedSender<FromCore>>,
    connections: u64,
}

impl Cores {
    fn lock(&self) -> MutexGuard<'_, Connected> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn connected(&self, machine: &str) -> bool {
        self.lock().cores.contains_key(machine)
    }

    pub fn send(&self, machine: &str, message: ToCore) -> bool {
        let cores = self.lock();
        let core = cores.cores.get(machine);
        core.is_some_and(|(_, core)| core.send(message).is_ok())
    }

    pub fn end(&self, machine: &str, why: &str) {
        let mut cores = self.lock();
        cores.cores.remove(machine);
        cores.terminals.retain(|(of, _), _| of != machine);
        let asked = cores.asks.extract_if(|_, (of, ..)| of == machine);
        for (_, (_, _, asker)) in asked {
            asker.send(Err(why.to_owned())).ok();
        }
    }

    pub fn open_terminal(
        &self,
        machine: &str,
        agent: Agent,
    ) -> Result<mpsc::UnboundedReceiver<FromCore>> {
        let mut cores = self.lock();
        if !cores.cores.contains_key(machine) {
            return Err(ApiError::invalid("This machine isn't connected"));
        }
        let key = (machine.to_owned(), agent);
        if cores
            .terminals
            .get(&key)
            .is_some_and(|open| !open.is_closed())
        {
            return Err(ApiError::invalid(
                "This sign-in is already open in another window",
            ));
        }
        let (to_browser, from_core) = mpsc::unbounded_channel();
        cores.terminals.insert(key, to_browser);
        Ok(from_core)
    }

    fn close_terminal(&self, machine: &str, agent: Agent) {
        self.lock().terminals.remove(&(machine.to_owned(), agent));
    }
}
