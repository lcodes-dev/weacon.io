//! `dev-tls <folder> [port] [server-port]`: answers TLS for `localhost` on `port` of 127.0.0.1,
//! by default 8443, and passes each connection on to the server on `server-port`, by default
//! 8080, until stopped. It makes its certificate in `<folder>` the first time, and prints its
//! address on one line once it listens; the core pins `<folder>/cert.pem`.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use weacon_testing::Failure;
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
    match run(dir, listen_on, server).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}

async fn run(dir: PathBuf, port: u16, server: u16) -> Result<(), Failure> {
    let config = server_config(&dir)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await?;
    let address = format!("https://localhost:{}\n", listener.local_addr()?.port());
    let mut stdout = tokio::io::stdout();
    stdout.write_all(address.as_bytes()).await?;
    stdout.flush().await?;
    front(
        listener,
        config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, server)),
    )
    .await?;
    Ok(())
}
