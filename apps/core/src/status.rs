use std::io::Write;

use weacon_appkit::build;
use weacon_observability::Fault;
use weacon_protocol::code;

use crate::app::{App, runtime};
use crate::daemon::socket_path;

/// Shakes hands with the daemon and prints its build.
pub(crate) fn run(app: &App, out: &mut impl Write) -> Result<(), Fault> {
    let state_dir = app.state_dir()?;
    let socket = socket_path(state_dir);
    let daemon = runtime()?.block_on(weacon_client::handshake(&socket, build(&app.version)))?;
    let offer = crate::update::offer(state_dir, &daemon.app.version);
    let lines = [
        Some(format!(
            "Daemon: running, weacon {} ({})",
            daemon.app.version, daemon.app.commit
        )),
        offer,
    ];
    lines
        .into_iter()
        .flatten()
        .try_for_each(|line| writeln!(out, "{line}"))
        .map_err(|error| {
            Fault::new(
                code!("status.output_failed"),
                "The status couldn't be printed",
            )
            .because(error)
        })
}
