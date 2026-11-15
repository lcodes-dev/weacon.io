use std::time::Duration;

use weacon_observability::{Deliver, Fault};
use weacon_protocol::code;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Sends reports over HTTPS, so their auth header never travels in the clear.
#[must_use]
pub fn deliver_over_https() -> Deliver {
    over(true)
}

fn over(https_only: bool) -> Deliver {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .https_only(https_only)
        // A redirect would carry the auth header to wherever it points.
        .max_redirects(0)
        .build()
        .into();
    Box::new(move |upload| {
        let response = agent
            .post(&upload.url)
            .header("X-Sentry-Auth", &upload.auth)
            .header("Content-Type", "application/x-sentry-envelope")
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
            url: format!("http://127.0.0.1:{port}/api/42/envelope/"),
            auth: "Sentry sentry_key=public".to_owned(),
            body: b"envelope".to_vec(),
        }
    }

    #[test]
    fn delivery_posts_the_envelope_with_its_auth() {
        let (port, server) = serve_once("200 OK".to_owned());
        over(false)(&upload(port)).unwrap();
        let (head, body) = server.join().unwrap();
        let has = |header: &str| head.iter().any(|line| line.eq_ignore_ascii_case(header));

        assert_eq!(head[0], "POST /api/42/envelope/ HTTP/1.1");
        assert!(has("x-sentry-auth: Sentry sentry_key=public"), "{head:?}");
        assert!(
            has("content-type: application/x-sentry-envelope"),
            "{head:?}"
        );
        assert_eq!(body, b"envelope");
    }

    #[test]
    fn delivery_fails_for_a_refused_report() {
        let (port, server) = serve_once("500 Internal Server Error".to_owned());
        let error = over(false)(&upload(port)).unwrap_err();
        drop(server.join().unwrap());

        assert_eq!(error.code().as_str(), "report.send_failed");
    }

    #[test]
    fn delivery_refuses_to_send_in_the_clear() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let error = deliver_over_https()(&upload(port)).unwrap_err();

        assert_eq!(error.code().as_str(), "report.send_failed");
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn a_redirect_is_a_failure_and_is_not_followed() {
        let elsewhere = TcpListener::bind("127.0.0.1:0").unwrap();
        elsewhere.set_nonblocking(true).unwrap();
        let target = elsewhere.local_addr().unwrap();
        let (port, server) = serve_once(format!("302 Found\r\nlocation: http://{target}/"));
        let error = over(false)(&upload(port)).unwrap_err();
        drop(server.join().unwrap());

        assert_eq!(error.code().as_str(), "report.send_failed");
        assert_eq!(
            elsewhere.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
