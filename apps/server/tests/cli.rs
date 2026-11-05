//! Runs the built `weacon-server` binary.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Child, Command, Output, Stdio};

    fn server() -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_weacon-server"));
        command.env_remove("WEACON_SENTRY_DSN");
        command
    }

    fn run(args: &[&str]) -> Output {
        server().args(args).output().unwrap()
    }

    // Stops the server even when the test fails first.
    struct Running(Child);

    impl Drop for Running {
        fn drop(&mut self) {
            drop(self.0.kill());
            drop(self.0.wait());
        }
    }

    #[test]
    fn version_prints_the_version_and_commit() {
        let output = run(&["--version"]);
        let text = String::from_utf8(output.stdout).unwrap();

        assert!(output.status.success());
        assert!(
            text.starts_with(&format!("weacon-server {} (", env!("CARGO_PKG_VERSION"))),
            "{text}"
        );
    }

    #[test]
    fn an_address_in_use_fails_with_its_code() {
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = taken.local_addr().unwrap().to_string();
        let output = server()
            .args(["serve", "--listen", &address])
            .env("WEACON_SENTRY_DSN", "")
            .output()
            .unwrap();
        let logs = String::from_utf8_lossy(&output.stdout);

        assert_eq!(output.status.code(), Some(2));
        assert!(logs.contains("server.bind_failed"), "{logs}");
        assert!(!logs.contains("report setup failed"), "{logs}");
    }

    #[test]
    fn serves_its_version_on_the_address_it_logs() {
        let mut child = Running(
            server()
                .args(["serve", "--listen", "127.0.0.1:0"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let lines = BufReader::new(child.0.stdout.take().unwrap()).lines();
        let address = lines
            .map(|line| serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap())
            .find(|record| record["msg"] == "listening")
            .unwrap()["address"]
            .as_str()
            .unwrap()
            .to_owned();
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .write_all(b"GET /version HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\r\n")
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
        assert!(
            response.contains("\"name\":\"weacon-server\""),
            "{response}"
        );
    }
}
