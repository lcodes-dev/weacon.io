use std::time::Duration;

use weacon_observability::{Deliver, Fault};
use weacon_protocol::code;

const TIMEOUT: Duration = Duration::from_secs(5);

/// POSTs each report to its OTLP endpoint as JSON.
#[must_use]
pub fn deliver_over_http() -> Deliver {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .build()
        .into();
    Box::new(move |upload| {
        let response = agent
            .post(&upload.url)
            .header("Content-Type", "application/json")
            .send(upload.body.as_slice())
            .map_err(|error| send_failed().because(error))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(send_failed())
        }
    })
}

fn send_failed() -> Fault {
    Fault::new(
        code!("report.send_failed"),
        "The error report couldn't be sent",
    )
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::thread;

    use weacon_observability::Upload;

    use super::*;

    type Request = (Vec<String>, Vec<u8>);

    // Answers one request with `status`, and hands back its head lines and body.
    fn serve_once(status: String) -> (u16, thread::JoinHandle<Request>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream);
            let head: Vec<String> = reader
                .by_ref()
                .lines()
                .map(Result::unwrap)
                .take_while(|line| !line.is_empty())
                .collect();
            let length = head
                .iter()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length: ")?
                        .parse()
                        .ok()
                })
                .unwrap_or(0);
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let reply = format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n");
            reader.get_mut().write_all(reply.as_bytes()).unwrap();
            (head, body)
        });
        (port, server)
    }

    fn upload(port: u16) -> Upload {
        Upload {
            url: format!("http://127.0.0.1:{port}/v1/logs"),
            body: b"{}".to_vec(),
        }
    }

    #[test]
    fn delivery_posts_the_report_as_json() {
        let (port, server) = serve_once("200 OK".to_owned());
        deliver_over_http()(&upload(port)).unwrap();
        let (head, body) = server.join().unwrap();
        let has = |header: &str| head.iter().any(|line| line.eq_ignore_ascii_case(header));

        assert_eq!(head[0], "POST /v1/logs HTTP/1.1");
        assert!(has("content-type: application/json"), "{head:?}");
        assert_eq!(body, b"{}");
    }

    #[test]
    fn delivery_fails_for_a_refused_report() {
        let (port, server) = serve_once("500 Internal Server Error".to_owned());
        let error = deliver_over_http()(&upload(port)).unwrap_err();
        drop(server.join().unwrap());

        assert_eq!(error.code().as_str(), "report.send_failed");
    }
}
