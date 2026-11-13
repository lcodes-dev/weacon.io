use axum::Router;
use axum::http::header;
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

/// Routes for those of `files` the server knows: `index.html` at `/`, the rest at their names.
pub(crate) fn routes(files: &'static [WebFile]) -> Router<Shared> {
    files.iter().fold(Router::new(), |router, &(name, bytes)| {
        let (path, kind) = match name {
            "index.html" => ("/", "text/html; charset=utf-8"),
            "app.js" => ("/app.js", "text/javascript; charset=utf-8"),
            "app.css" => ("/app.css", "text/css; charset=utf-8"),
            _ => return router,
        };
        router.route(path, get(move || async move { file(kind, bytes) }))
    })
}

fn file(kind: &'static str, bytes: &'static [u8]) -> Response {
    (
        [
            (header::CONTENT_TYPE, kind),
            // No validator is sent, so each load fetches the file whole; it is small.
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONTENT_SECURITY_POLICY, POLICY),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        bytes,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use crate::test_support::{Fixture, header, request, run, status};

    #[tokio::test]
    async fn each_built_file_is_served_with_its_type_and_no_cache() {
        let fixture = Fixture::new().await;
        let server = run(fixture.shared.clone()).await;
        let mut kinds = Vec::new();
        for (path, body) in [
            ("/", "<!doctype html><title>Made up</title>"),
            ("/app.js", "made_up();"),
            ("/app.css", "body {}"),
        ] {
            let (head, sent) = request(server.address, &format!("GET {path}"), "").await;
            assert_eq!(status(&head), "http/1.1 200 ok", "{path}");
            assert_eq!(sent, body);
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
    async fn only_the_built_files_the_server_knows_are_served() {
        let mut fixture = Fixture::new().await;
        fixture.shared.web = &[("app.js", b"made_up();"), ("app.wasm", b"\0asm")];
        let server = run(fixture.shared.clone()).await;
        let mut statuses = Vec::new();
        for path in ["/", "/index.html", "/app.js", "/app.wasm", "/app.map"] {
            let (head, _) = request(server.address, &format!("GET {path}"), "").await;
            statuses.push(status(&head).to_owned());
        }
        let missing = "http/1.1 404 not found";

        assert_eq!(
            statuses,
            [missing, missing, "http/1.1 200 ok", missing, missing]
        );
    }
}
