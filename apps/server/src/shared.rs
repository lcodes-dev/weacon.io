use std::sync::Arc;
use std::time::UNIX_EPOCH;

use tokio::sync::broadcast;
use weacon_observability::{Clock, Logger, Reporter};
use weacon_protocol::{App, MachineId, WorkspaceId};

use crate::chats;
use crate::db::Db;
use crate::links::Links;
use crate::relay::Relay;
use crate::secret::Secret;
use crate::socket;
use crate::terminals::Terminals;
use crate::web::WebFile;

#[derive(Clone)]
pub(crate) struct Shared {
    pub(crate) app: App,
    pub(crate) log: Logger,
    pub(crate) reporter: Arc<Reporter>,
    pub(crate) db: Db,
    pub(crate) secret: Arc<Secret>,
    pub(crate) clock: Arc<Clock>,
    pub(crate) sockets: socket::Timing,
    /// The machines whose connections are open.
    pub(crate) links: Arc<Links>,
    /// The asks waiting for their machines' answers.
    pub(crate) relay: Arc<Relay>,
    /// The sign-in terminals open now.
    pub(crate) terminals: Arc<Terminals>,
    /// Which machine was just unpaired, for its open connection.
    pub(crate) unpaired: broadcast::Sender<MachineId>,
    /// Which workspace just changed, for the feeds.
    pub(crate) changes: broadcast::Sender<WorkspaceId>,
    pub(crate) chats: chats::Hub,
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
