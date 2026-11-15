//! `dev-tls <folder> [port] [server-port]`: answers TLS for `localhost` on `port` of 127.0.0.1,
//! by default 8443, and passes each connection on to the server on `server-port`, by default
//! 8080, until stopped. It makes its certificate in `<folder>` the first time, and prints its
//! address on one line once it listens; the core pins `<folder>/cert.pem`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use weacon_testing::dev_tls::{front, server_config};

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(dir) = args.next().map(PathBuf::from) else {
        return ExitCode::from(2);
    };
    let mut port = |default| args.next().map_or(Ok(default), |arg| arg.parse::<u16>());
    let (Ok(listen_on), Ok(server)) = (port(8443), port(8080)) else {
        return ExitCode::from(2);
    };
    let Err(why) = run(dir, listen_on, server).await else {
        return ExitCode::SUCCESS;
    };
    let mut stderr = tokio::io::stderr();
    // Best effort: the exit code says it failed either way.
    drop(
        stderr
            .write_all(format!("dev-tls: {why}\n").as_bytes())
            .await,
    );
    ExitCode::FAILURE
}

/// Runs the front until it fails, and says which step failed.
async fn run(dir: PathBuf, port: u16, server: u16) -> Result<(), &'static str> {
    let config = server_config(&dir).map_err(|_| "its certificate folder can't be used")?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .await
        .map_err(|_| "it can't listen on its port; is another one running?")?;
    let address = format!(
        "https://localhost:{port}\n",
        port = listener
            .local_addr()
            .map_err(|_| "it can't listen on its port")?
            .port()
    );
    let mut stdout = tokio::io::stdout();
    stdout
        .write_all(address.as_bytes())
        .await
        .map_err(|_| "it can't print its address")?;
    stdout
        .flush()
        .await
        .map_err(|_| "it can't print its address")?;
    front(
        listener,
        config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, server)),
    )
    .await
    .map_err(|_| "it stopped accepting connections")
}
