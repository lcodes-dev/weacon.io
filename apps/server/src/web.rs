use axum::Router;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

use crate::http::Shared;

/// A built file of the web app: its name and its bytes.
pub(crate) type WebFile = (&'static str, &'static [u8]);

/// The web app's built files, embedded when the server was built; none if it wasn't.
pub(crate) const FILES: &[WebFile] = include!(concat!(env!("OUT_DIR"), "/web.rs"));

// Only the server's own files and its API, over its own origin.
const POLICY: &str = "default-src 'self'; base-uri 'none'; form-action 'none'; \
                      frame-ancestors 'none'";

/// Routes for `files`: `index.html` at `/`, the rest at their names.
pub(crate) fn routes(files: &'static [WebFile]) -> Router<Shared> {
    files.iter().fold(Router::new(), |router, &(name, bytes)| {
        let path = match name {
            "index.html" => "/".to_owned(),
            _ => format!("/{name}"),
        };
        router.route(&path, get(move || async move { file(name, bytes) }))
    })
}

fn file(name: &str, bytes: &'static [u8]) -> Response {
    let kind = match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(kind)),
            // The names never change, so each load asks whether the file has.
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(POLICY),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use crate::test_support::{self, Fixture, run, status};

    async fn get(fixture: &Fixture, path: &str) -> (Vec<String>, String) {
        let server = run(fixture.shared.clone()).await;
        test_support::request(server.address, &format!("GET {path}"), "").await
    }

    fn header<'a>(head: &'a [String], name: &str) -> Option<&'a str> {
        head.iter()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(": "))
    }

    #[tokio::test]
    async fn each_built_file_is_served_with_its_type_and_no_cache() {
        let fixture = Fixture::new().await;
        let mut kinds = Vec::new();
        for (path, body) in [
            ("/", "<!doctype html><title>Made up</title>"),
            ("/app.js", "made_up();"),
            ("/app.css", "body {}"),
        ] {
            let (head, served) = get(&fixture, path).await;
            assert_eq!(status(&head), "http/1.1 200 ok", "{path}");
            assert_eq!(served, body);
            assert_eq!(header(&head, "cache-control"), Some("no-cache"));
            assert_eq!(header(&head, "x-content-type-options"), Some("nosniff"));
            assert_eq!(header(&head, "referrer-policy"), Some("no-referrer"));
            assert!(
                header(&head, "content-security-policy")
                    .is_some_and(|policy| policy.starts_with("default-src 'self';"))
            );
            kinds.push(header(&head, "content-type").unwrap_or_default().to_owned());
        }

        assert_eq!(
            kinds,
            [
                "text/html; charset=utf-8",
                "text/javascript; charset=utf-8",
                "text/css; charset=utf-8",
            ]
        );
    }

    #[tokio::test]
    async fn only_the_built_files_are_served() {
        let mut fixture = Fixture::new().await;
        let (index, _) = get(&fixture, "/index.html").await;
        let (other, _) = get(&fixture, "/app.map").await;
        fixture.shared.web = &[("app.wasm", b"\0asm")];
        let (unbuilt, _) = get(&fixture, "/").await;
        let (unknown, _) = get(&fixture, "/app.wasm").await;

        assert_eq!(status(&index), "http/1.1 404 not found");
        assert_eq!(status(&other), "http/1.1 404 not found");
        assert_eq!(status(&unbuilt), "http/1.1 404 not found");
        assert_eq!(
            header(&unknown, "content-type"),
            Some("application/octet-stream")
        );
    }
}
