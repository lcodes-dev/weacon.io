//! Runs the built `weacon-server` binary.

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::process::{Child, Command, Output, Stdio};
    use std::sync::mpsc::{Receiver, RecvTimeoutError};

    use weacon_testing::TempDir;

    fn server() -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_weacon-server"));
        command.env_remove("OTEL_EXPORTER_OTLP_ENDPOINT");
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

    // A command that should end: its exit code and logs, or a failed test after 30 seconds.
    fn exits(command: &mut Command) -> (Option<i32>, String) {
        let mut child = Running(command.stdout(Stdio::piped()).spawn().unwrap());
        let mut stdout = child.0.stdout.take().unwrap();
        let (sent, logs) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut text = String::new();
            drop(stdout.read_to_string(&mut text));
            drop(sent.send(text));
        });
        let logs = logs
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the server didn't stop");
        (child.0.wait().unwrap().code(), logs)
    }

    #[test]
    fn an_address_in_use_fails_with_its_code() {
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = taken.local_addr().unwrap().to_string();
        let db = TempDir::new().unwrap();
        let (code, logs) = exits(
            server()
                .args(["serve", "--listen", &address, "--db"])
                .arg(db.join("weacon.db")),
        );

        assert_eq!(code, Some(2));
        assert!(logs.contains("server.bind_failed"), "{logs}");
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

    // A server on `db`, its address, and its log lines as they come.
    fn serve(db: &TempDir) -> (Running, String, Receiver<String>) {
        let mut child = Running(
            server()
                .args(["serve", "--listen", "127.0.0.1:0", "--db"])
                .arg(db.join("weacon.db"))
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let stdout = BufReader::new(child.0.stdout.take().unwrap());
        let (sent, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in stdout.lines().map_while(Result::ok) {
                drop(sent.send(line));
            }
        });
        let address = loop {
            let record = parsed(&next(&lines).expect("the server didn't listen"));
            if record["msg"] == "listening" {
                break record["address"].as_str().unwrap().to_owned();
            }
        };
        (child, address, lines)
    }

    // The next log line, or `None` once the server has stopped; a failed test after 30 seconds.
    fn next(lines: &Receiver<String>) -> Option<String> {
        match lines.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(line) => Some(line),
            Err(RecvTimeoutError::Disconnected) => None,
            Err(RecvTimeoutError::Timeout) => panic!("no log line came"),
        }
    }

    fn parsed(line: &str) -> serde_json::Value {
        serde_json::from_str(line).unwrap()
    }

    // The next mail the server prints, keeping the log lines before it in `logs`.
    fn mail(lines: &Receiver<String>, logs: &mut String) -> serde_json::Value {
        loop {
            let line = next(lines).expect("no mail came");
            let record = parsed(&line);
            if record["msg"] == "mail printed" {
                return record;
            }
            logs.push_str(&line);
        }
    }

    // The code in a sign-in mail's text: its one word with a dash in the middle.
    fn code_in(text: &str) -> &str {
        text.split(|letter: char| !letter.is_ascii_alphanumeric() && letter != '-')
            .find(|word| word.len() == 9 && word.get(4..5) == Some("-"))
            .unwrap()
    }

    #[test]
    fn a_member_signs_in_with_the_code_in_the_mail_it_prints() {
        let db = TempDir::new().unwrap();
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
        let (child, address, lines) = serve(&db);
        let mut logs = String::new();
        let version = http(&address, "GET /version", "");
        let start = r#"{"email":"ada@example.com"}"#;
        let started = http(&address, "POST /api/sign-in", start);
        let challenge = field(&started, "challenge");
        let mail = mail(&lines, &mut logs);
        let code = code_in(mail["text"].as_str().unwrap());
        let finish = serde_json::json!({
            "email": "ada@example.com",
            "challenge": challenge,
            "code": code,
        })
        .to_string();
        let finished = http(&address, "POST /api/sign-in/finish", &finish);
        let token = field(&finished, "token").as_str().unwrap().to_owned();
        drop(child);
        while let Some(line) = next(&lines) {
            logs.push_str(&line);
        }

        assert!(version.starts_with("HTTP/1.1 200 OK"), "{version}");
        assert!(version.contains("\"name\":\"weacon-server\""), "{version}");
        assert!(started.starts_with("HTTP/1.1 202 Accepted"), "{started}");
        assert_eq!(mail["to"], "ada@example.com");
        assert!(finished.starts_with("HTTP/1.1 200 OK"), "{finished}");
        assert_eq!(token.len(), 64);
        assert!(logs.contains("request handled"), "{logs}");
        // Only the mail holds them. A code is short, so it is looked for as a whole JSON string.
        let challenge = challenge.as_str().unwrap();
        let bare = format!("\"{}\"", code.replace('-', ""));
        for secret in [
            "ada@example.com",
            &format!("\"{code}\""),
            &bare,
            &token,
            challenge,
        ] {
            assert!(!logs.contains(secret), "{logs}");
        }
    }

    // The field `name` of an HTTP answer's JSON body.
    fn field(answer: &str, name: &str) -> serde_json::Value {
        let (_, body) = answer.split_once("\r\n\r\n").unwrap();
        serde_json::from_str::<serde_json::Value>(body).unwrap()[name].clone()
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
}
