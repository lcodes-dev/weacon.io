use std::io::Write;

use weacon_observability::Fault;
use weacon_protocol::code;

use crate::app::{App, state_unavailable};
use crate::daemon::{build, runtime, socket_path};

/// Shakes hands with the daemon and prints its build.
pub(crate) fn run(app: &App, out: &mut impl Write) -> Result<(), Fault> {
    let state_dir = app.state_dir.as_deref().ok_or_else(state_unavailable)?;
    let socket = socket_path(state_dir);
    let daemon = runtime()?.block_on(weacon_client::handshake(&socket, build(&app.version)))?;
    writeln!(
        out,
        "Daemon: running, weacon {} ({})",
        daemon.app.version, daemon.app.commit
    )
    .map_err(|error| {
        Fault::new(
            code!("status.output_failed"),
            "The status couldn't be printed",
        )
        .because(error)
    })
}
