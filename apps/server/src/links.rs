//! The machines whose connections are open: the one way the server sends a machine anything.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::{broadcast, mpsc};
use weacon_protocol::{AccountId, Capability, Envelope, MachineId};

/// How many messages wait for a machine's connection to take them.
pub(crate) const QUEUE: usize = 64;
/// How many connections a subscriber to [`Links::connected`] may fall behind by.
const CONNECTED: usize = 64;

pub(crate) struct Links {
    open: Mutex<HashMap<MachineId, Open>>,
    connected: broadcast::Sender<Connected>,
}

struct Open {
    to_machine: mpsc::Sender<Envelope>,
    offers: Vec<Capability>,
}

/// A machine whose connection just opened, and whose machine it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Connected {
    pub(crate) machine: MachineId,
    pub(crate) owner: AccountId,
}

/// The machine isn't connected, or doesn't keep up with what it's sent.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Offline;

/// A machine's connection while it is open in [`Links`]; dropping it closes it there.
pub(crate) struct Attached {
    links: Arc<Links>,
    machine: MachineId,
    to_machine: mpsc::Sender<Envelope>,
    /// What the server sends the machine.
    pub(crate) incoming: mpsc::Receiver<Envelope>,
}

impl Default for Links {
    fn default() -> Self {
        Self {
            open: Mutex::default(),
            connected: broadcast::channel(CONNECTED).0,
        }
    }
}

impl Links {
    /// Opens `connected`'s machine, whose core `offers` these capabilities, and tells the
    /// subscribers; a newer connection of the same machine takes over from an older one.
    pub(crate) fn open(
        self: &Arc<Self>,
        connected: Connected,
        offers: Vec<Capability>,
    ) -> Attached {
        let (to_machine, incoming) = mpsc::channel(QUEUE);
        let open = Open {
            to_machine: to_machine.clone(),
            offers,
        };
        self.lock().insert(connected.machine.clone(), open);
        let machine = connected.machine.clone();
        // No subscriber is no one to tell.
        drop(self.connected.send(connected));
        Attached {
            links: Arc::clone(self),
            machine,
            to_machine,
            incoming,
        }
    }

    /// Sends `envelope` to `machine` over its open connection.
    ///
    /// # Errors
    /// [`Offline`] when it isn't connected, or its queue is full.
    pub(crate) fn send(&self, machine: &MachineId, envelope: Envelope) -> Result<(), Offline> {
        let open = self.lock();
        let link = open.get(machine).ok_or(Offline)?;
        link.to_machine.try_send(envelope).map_err(|_| Offline)
    }

    /// Whether `machine` is connected, and its core offers `capability`.
    pub(crate) fn offers(&self, machine: &MachineId, capability: &Capability) -> bool {
        self.lock()
            .get(machine)
            .is_some_and(|link| link.offers.contains(capability))
    }

    /// Each machine connected from now on.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "chats subscribe to it to resend a machine's open requests"
        )
    )]
    pub(crate) fn connected(&self) -> broadcast::Receiver<Connected> {
        self.connected.subscribe()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<MachineId, Open>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        let mut open = self.links.lock();
        let ours = open
            .get(&self.machine)
            .is_some_and(|link| link.to_machine.same_channel(&self.to_machine));
        if ours {
            open.remove(&self.machine);
        }
    }
}

#[cfg(test)]
mod tests {
    use weacon_protocol::{Message, code};

    use super::*;

    fn machine(id: &str) -> MachineId {
        MachineId::try_from(id.to_owned()).unwrap()
    }

    fn connected(id: &str) -> Connected {
        Connected {
            machine: machine(id),
            owner: AccountId::try_from("a-1".to_owned()).unwrap(),
        }
    }

    fn envelope(id: u32) -> Envelope {
        Envelope {
            id,
            message: Message::Unknown,
        }
    }

    #[test]
    fn only_an_open_machine_is_sent_to_and_it_says_what_it_offers() {
        let links = Arc::new(Links::default());
        let settings = Capability::from(code!("machine.settings"));
        let mut subscriber = links.connected();

        let not_yet = links.send(&machine("mc-1"), envelope(1));
        let mut attached = links.open(connected("mc-1"), vec![settings.clone()]);
        let _bare = links.open(connected("mc-2"), Vec::new());

        assert_eq!(not_yet, Err(Offline));
        assert_eq!(links.send(&machine("mc-1"), envelope(2)), Ok(()));
        assert_eq!(attached.incoming.try_recv().unwrap(), envelope(2));
        assert!(links.offers(&machine("mc-1"), &settings));
        assert!(!links.offers(&machine("mc-2"), &settings));
        assert!(!links.offers(&machine("mc-3"), &settings));
        assert_eq!(subscriber.try_recv().unwrap(), connected("mc-1"));
        assert_eq!(subscriber.try_recv().unwrap(), connected("mc-2"));
    }

    #[test]
    fn a_newer_connection_takes_over_and_an_older_one_leaving_keeps_it() {
        let links = Arc::new(Links::default());

        let older = links.open(connected("mc-1"), Vec::new());
        let newer = links.open(connected("mc-1"), Vec::new());
        drop(older);
        let still = links.send(&machine("mc-1"), envelope(1));
        drop(newer);

        assert_eq!(still, Ok(()));
        assert_eq!(links.send(&machine("mc-1"), envelope(2)), Err(Offline));
    }

    #[test]
    fn a_machine_that_doesnt_keep_up_is_offline_until_it_does() {
        let links = Arc::new(Links::default());
        let mut attached = links.open(connected("mc-1"), Vec::new());

        let sent: Vec<_> = (0..=QUEUE)
            .map(|id| links.send(&machine("mc-1"), envelope(u32::try_from(id).unwrap())))
            .collect();
        drop(attached.incoming.try_recv());

        assert!(sent.iter().take(QUEUE).all(Result::is_ok));
        assert_eq!(sent.last(), Some(&Err(Offline)));
        assert_eq!(links.send(&machine("mc-1"), envelope(99)), Ok(()));
    }
}
