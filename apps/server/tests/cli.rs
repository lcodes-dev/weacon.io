//! Runs the built `weacon-server` binary.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Lines, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Child, ChildStdout, Command, Output, Stdio};

    use weacon_testing::TempDir;

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
        let db = TempDir::new().unwrap();
        let output = server()
            .args(["serve", "--listen", &address, "--db"])
            .arg(db.join("weacon.db"))
            .arg("--outbox")
            .arg(&*db)
            .env("WEACON_SENTRY_DSN", "")
            .output()
            .unwrap();
        let logs = String::from_utf8_lossy(&output.stdout);

        assert_eq!(output.status.code(), Some(2));
        assert!(logs.contains("server.bind_failed"), "{logs}");
        assert!(!logs.contains("report setup failed"), "{logs}");
    }

    // One HTTP/1.1 exchange with a JSON body; the whole response.
    fn http(address: &str, head: &str, body: &str) -> String {
        let mut stream = TcpStream::connect(address).unwrap();
        let request = format!(
            "{head} HTTP/1.1\r\nhost: test\r\nconnection: close\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(30)))
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        response
    }

    // A server on `db` and `outbox`, its address, and the rest of its logs.
    fn serve(db: &TempDir, outbox: &TempDir) -> (Running, String, Lines<BufReader<ChildStdout>>) {
        let mut child = Running(
            server()
                .args(["serve", "--listen", "127.0.0.1:0", "--db"])
                .arg(db.join("weacon.db"))
                .arg("--outbox")
                .arg(&**outbox)
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut lines = BufReader::new(child.0.stdout.take().unwrap()).lines();
        let address = lines
            .by_ref()
            .map(|line| serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap())
            .find(|record| record["msg"] == "listening")
            .unwrap()["address"]
            .as_str()
            .unwrap()
            .to_owned();
        (child, address, lines)
    }

    // The code in the letter the server leaves in `outbox` after answering, and who it is for.
    fn letter(outbox: &TempDir) -> (String, String) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let file = loop {
            assert!(std::time::Instant::now() < deadline, "no letter came");
            let posted = std::fs::read_dir(outbox)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "json")
                });
            if let Some(file) = posted {
                break file;
            }
            std::thread::yield_now();
        };
        let letter: serde_json::Value =
            serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        let text = |key: &str| letter[key].as_str().unwrap().to_owned();
        (text("to"), text("code"))
    }

    #[test]
    fn a_member_signs_in_with_a_code_from_the_outbox() {
        let db = TempDir::new().unwrap();
        let outbox = TempDir::new().unwrap();
        let workspace = admin(&["create-workspace", "--name", "Team"], &db);
        admin(
            &[
                "add-member",
                "--workspace",
                &workspace,
                "--email",
                "ada@example.com",
            ],
            &db,
        );
        let (child, address, lines) = serve(&db, &outbox);
        let version = http(&address, "GET /version", "");
        let start = format!(r#"{{"workspace":"{workspace}","email":"ada@example.com"}}"#);
        let started = http(&address, "POST /sign-in", &start);
        let (_, started_body) = started.split_once("\r\n\r\n").unwrap();
        let challenge =
            serde_json::from_str::<serde_json::Value>(started_body).unwrap()["challenge"].clone();
        let (to, code) = letter(&outbox);
        let finish = serde_json::json!({
            "workspace": workspace,
            "email": "ada@example.com",
            "challenge": challenge,
            "code": code,
        })
        .to_string();
        let finished = http(&address, "POST /sign-in/finish", &finish);
        let (_, body) = finished.split_once("\r\n\r\n").unwrap();
        let token = serde_json::from_str::<serde_json::Value>(body).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_owned();
        drop(child);
        let logs: String = lines.map(Result::unwrap).collect();

        assert!(version.starts_with("HTTP/1.1 200 OK"), "{version}");
        assert!(version.contains("\"name\":\"weacon-server\""), "{version}");
        assert!(started.starts_with("HTTP/1.1 202 Accepted"), "{started}");
        assert_eq!(to, "ada@example.com");
        assert!(finished.starts_with("HTTP/1.1 200 OK"), "{finished}");
        assert_eq!(token.len(), 64);
        assert!(logs.contains("request handled"), "{logs}");
        // A code is eight digits, so it is looked for as a whole JSON string.
        let challenge = challenge.as_str().unwrap();
        for secret in ["ada@example.com", &format!("\"{code}\""), &token, challenge] {
            assert!(!logs.contains(secret), "{logs}");
        }
    }

    fn admin(args: &[&str], db: &TempDir) -> String {
        let output = server()
            .args(args)
            .arg("--db")
            .arg(db.join("weacon.db"))
            .current_dir(db)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    #[test]
    fn a_backup_restores_a_workspace() {
        let db = TempDir::new().unwrap();
        let workspace = admin(&["create-workspace", "--name", "Team"], &db);
        let member = admin(
            &[
                "add-member",
                "--workspace",
                &workspace,
                "--email",
                "ada@example.com",
            ],
            &db,
        );
        admin(&["backup", "--to", "backup.db"], &db);
        let restored = TempDir::new().unwrap();
        let backup = db.join("backup.db");
        admin(&["restore", "--from", backup.to_str().unwrap()], &restored);

        assert_eq!(
            admin(&["members", "--workspace", &workspace], &restored),
            format!("{member} ada@example.com")
        );
    }
}
