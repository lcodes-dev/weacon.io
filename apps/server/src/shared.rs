use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use tokio::sync::{Semaphore, broadcast};
use weacon_observability::{Clock, Logger, Reporter};
use weacon_protocol::{App, MachineId, WorkspaceId};

use crate::client::Net;
use crate::db::Db;
use crate::links::Links;
use crate::outbox::Outbox;
use crate::pairing;
use crate::relay::Relay;
use crate::secret::Secret;
use crate::sign_in;
use crate::sign_up::SignUp;
use crate::socket;
use crate::web::WebFile;

#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) app: App,
    pub(crate) log: Logger,
    pub(crate) reporter: Arc<Reporter>,
    pub(crate) db: Db,
    pub(crate) outbox: Outbox,
    pub(crate) secret: Arc<Secret>,
    pub(crate) clock: Arc<Clock>,
    /// How long a connection may take to send a request's head, or sit idle between requests.
    pub(crate) head_within: Duration,
    /// Proxies whose `X-Forwarded-For` is believed.
    pub(crate) proxies: Arc<[Net]>,
    /// Room for open connections, feeds included; when it's full, more wait to be accepted.
    pub(crate) connections: Arc<Semaphore>,
    pub(crate) sockets: socket::Timing,
    /// The machines whose connections are open.
    pub(crate) links: Arc<Links>,
    /// The asks waiting for their machines' answers.
    pub(crate) relay: Arc<Relay>,
    /// Which machine was just unpaired, for its open connection.
    pub(crate) unpaired: broadcast::Sender<MachineId>,
    /// Which workspace just changed, for the feeds.
    pub(crate) changes: broadcast::Sender<WorkspaceId>,
    /// Room for sign-ins being worked on after their answer; when it's full, more are dropped.
    pub(crate) sign_ins: Arc<Semaphore>,
    /// How long finishing a sign-in takes at least, so its timing gives nothing away.
    pub(crate) finish_takes: Duration,
    /// Who may make an account by signing in.
    pub(crate) sign_up: Arc<[SignUp]>,
    pub(crate) sign_in: Arc<sign_in::Limits>,
    pub(crate) pairing: Arc<pairing::Limits>,
    /// The web app's built files it serves.
    pub(crate) web: &'static [WebFile],
}

impl Shared {
    /// Seconds since the Unix epoch.
    pub(crate) fn now(&self) -> i64 {
        (self.clock)()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |since| {
                i64::try_from(since.as_secs()).unwrap_or(i64::MAX)
            })
    }

    /// Tells the open feeds that `workspace` changed; nobody listening is fine, as feeds read
    /// what they missed from the database.
    pub(crate) fn wake_feeds(&self, workspace: &WorkspaceId) {
        drop(self.changes.send(workspace.clone()));
    }
}
