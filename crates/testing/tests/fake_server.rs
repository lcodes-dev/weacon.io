//! Runs the built `fake-server` binary.

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::{Command, Stdio};

    use futures_util::{SinkExt, StreamExt};
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;
    use weacon_protocol::{PROTOCOL, code};
    use weacon_testing::TempDir;
    use weacon_testing::fake_core::client_hello;

    fn fake_server(args: &[&str]) -> Option<i32> {
        Command::new(env!("CARGO_BIN_EXE_fake-server"))
            .args(args)
            .status()
            .unwrap()
            .code()
    }

    #[test]
    fn without_a_folder_or_with_a_bad_number_it_says_how_to_run_it() {
        assert_eq!(fake_server(&[]), Some(2));
        assert_eq!(fake_server(&["/tmp", "5O"]), Some(2));
    }

    #[test]
    fn a_folder_it_cant_write_in_is_a_failure() {
        assert_eq!(fake_server(&["/nonexistent/weacon-fake-server"]), Some(1));
    }

    async fn welcome(url: &str) -> serde_json::Value {
        let hello = client_hello(1, PROTOCOL, [code!("run.stream")])
            .to_json()
            .unwrap();
        let (mut client, _) = connect_async(url).await.unwrap();
        client
            .send(Message::text(String::from_utf8(hello).unwrap()))
            .await
            .unwrap();
        let Message::Text(welcome) = client.next().await.unwrap().unwrap() else {
            panic!("no welcome");
        };
        client.close(None).await.unwrap();
        serde_json::from_str(welcome.as_str()).unwrap()
    }

    #[tokio::test]
    async fn it_prints_a_loopback_address_and_serves_one_client_after_another() {
        let dir = TempDir::new().unwrap();
        fs::write(dir.join("received.jsonl"), "stale\n").unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_fake-server"))
            .args([dir.to_str().unwrap(), "10", "0"])
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut url = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut url)
            .await
            .unwrap();
        let welcomes = [welcome(url.trim()).await, welcome(url.trim()).await];
        child.kill().await.unwrap();

        assert!(url.starts_with("ws://127.0.0.1:"), "{url}");
        assert!(welcomes.iter().all(|welcome| welcome["type"] == "welcome"));
        assert_eq!(fs::read_to_string(dir.join("received.jsonl")).unwrap(), "");
    }
}
