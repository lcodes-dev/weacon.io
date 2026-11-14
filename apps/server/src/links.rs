//! The machines whose connections are open: the one way the server sends a machine anything.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
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
    last_id: AtomicU32,
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
            last_id: AtomicU32::new(0),
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

    /// Sends `envelope` to `machine` over its open connection, if its core offers `needs`.
    ///
    /// # Errors
    /// [`Offline`] when it isn't connected, its core doesn't offer `needs`, or its queue is
    /// full.
    pub(crate) fn send(
        &self,
        machine: &MachineId,
        needs: &Capability,
        envelope: Envelope,
    ) -> Result<(), Offline> {
        let open = self.lock();
        let link = open
            .get(machine)
            .filter(|link| link.offers.contains(needs))
            .ok_or(Offline)?;
        link.to_machine.try_send(envelope).map_err(|_| Offline)
    }

    /// The id for an envelope sent to a machine, unique among those its answers may name for
    /// a long while; never 0, which the server and the core use for messages of their own.
    pub(crate) fn next_id(&self) -> u32 {
        self.last_id
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
            .max(1)
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

    fn settings() -> Capability {
        Capability::from(code!("machine.settings"))
    }

    fn sent(links: &Links, to: &str, id: u32) -> Result<(), Offline> {
        links.send(&machine(to), &settings(), envelope(id))
    }

    #[test]
    fn only_an_open_machine_that_offers_what_a_message_needs_is_sent_it() {
        let links = Arc::new(Links::default());
        let mut subscriber = links.connected();

        let not_yet = sent(&links, "mc-1", 1);
        let mut attached = links.open(connected("mc-1"), vec![settings()]);
        let mut bare = links.open(connected("mc-2"), Vec::new());

        assert_eq!(not_yet, Err(Offline));
        assert_eq!(sent(&links, "mc-1", 2), Ok(()));
        assert_eq!(attached.incoming.try_recv().unwrap(), envelope(2));
        assert_eq!(sent(&links, "mc-2", 3), Err(Offline));
        bare.incoming.try_recv().unwrap_err();
        assert_eq!(sent(&links, "mc-3", 4), Err(Offline));
        assert_eq!(subscriber.try_recv().unwrap(), connected("mc-1"));
        assert_eq!(subscriber.try_recv().unwrap(), connected("mc-2"));
    }

    #[test]
    fn ids_count_up_and_skip_0_as_they_wrap() {
        let links = Links::default();

        let first = [links.next_id(), links.next_id()];
        links.last_id.store(u32::MAX - 1, Ordering::Relaxed);
        let wrapped = [links.next_id(), links.next_id(), links.next_id()];

        assert_eq!(first, [1, 2]);
        assert_eq!(wrapped, [u32::MAX, 1, 1]);
    }

    #[test]
    fn a_newer_connection_takes_over_and_an_older_one_leaving_keeps_it() {
        let links = Arc::new(Links::default());

        let older = links.open(connected("mc-1"), vec![settings()]);
        let newer = links.open(connected("mc-1"), vec![settings()]);
        drop(older);
        let still = sent(&links, "mc-1", 1);
        drop(newer);

        assert_eq!(still, Ok(()));
        assert_eq!(sent(&links, "mc-1", 2), Err(Offline));
        assert!(links.lock().is_empty());
    }

    #[test]
    fn a_machine_that_doesnt_keep_up_is_offline_until_it_does() {
        let links = Arc::new(Links::default());
        let mut attached = links.open(connected("mc-1"), vec![settings()]);

        let queued: Vec<_> = (0..=QUEUE)
            .map(|id| sent(&links, "mc-1", u32::try_from(id).unwrap()))
            .collect();
        drop(attached.incoming.try_recv());

        assert!(queued.iter().take(QUEUE).all(Result::is_ok));
        assert_eq!(queued.last(), Some(&Err(Offline)));
        assert_eq!(sent(&links, "mc-1", 99), Ok(()));
    }
}
