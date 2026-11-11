use std::net::IpAddr;

use axum::extract::{FromRequestParts, RawPathParams};
use axum::http::header::{AUTHORIZATION, COOKIE, HOST, ORIGIN};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use weacon_protocol::{AccountId, MemberId, WorkspaceId, code};

use crate::db::Workspace;
use crate::email::Email;
use crate::http::{Shared, cross_site, failure, refused};
use crate::sign_in::token_hash;

/// The browser's session cookie; the prefix ties it to this host.
pub(crate) const SESSION_COOKIE: &str = "__Host-weacon_session";

/// A `Set-Cookie` value that keeps `token` for `lasts` seconds, out of reach of the page's
/// scripts and of other sites' requests.
pub(crate) fn set_session_cookie(token: &str, lasts: i64) -> String {
    format!("{SESSION_COOKIE}={token}; Path=/; Max-Age={lasts}; HttpOnly; Secure; SameSite=Strict")
}

/// Who is signed in.
pub(crate) struct Session {
    pub(crate) account: AccountId,
    pub(crate) email: Email,
}

/// Who asks for something in the workspace a route names as `{workspace}`: a member of it,
/// signed in. Anyone else is refused the same way, whether or not there is such a workspace.
pub(crate) struct Member {
    pub(crate) workspace: Workspace,
    pub(crate) id: MemberId,
    /// The hash of the session token that proved it.
    pub(crate) token_hash: [u8; 32],
}

impl FromRequestParts<Shared> for Session {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, shared: &Shared) -> Result<Self, Response> {
        let token_hash = proof(&parts.method, &parts.headers)?;
        let found = shared
            .db
            .session(&token_hash, shared.now())
            .await
            .map_err(|fault| failure(shared, fault))?;
        let (account, email) = found.ok_or_else(required)?;
        Ok(Self { account, email })
    }
}

impl FromRequestParts<Shared> for Member {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, shared: &Shared) -> Result<Self, Response> {
        let bad_request = |_| StatusCode::BAD_REQUEST.into_response();
        let params = RawPathParams::from_request_parts(parts, shared)
            .await
            .map_err(bad_request)?;
        let workspace = params
            .iter()
            .find(|(name, _)| *name == "workspace")
            .and_then(|(_, id)| WorkspaceId::try_from(id.to_owned()).ok())
            .ok_or_else(|| StatusCode::BAD_REQUEST.into_response())?;
        let token_hash = proof(&parts.method, &parts.headers)?;
        let found = shared
            .db
            .member_workspace(&workspace, &token_hash, shared.now())
            .await
            .map_err(|fault| failure(shared, fault))?;
        let (workspace, id) = found.ok_or_else(required)?;
        Ok(Self {
            workspace,
            id,
            token_hash,
        })
    }
}

fn required() -> Response {
    refused(code!("sign_in.required"))
}

/// Why a request carries no usable session token.
pub(crate) enum Unproven {
    /// It carries none.
    Missing,
    /// It carries the cookie, but isn't from the server's own pages.
    CrossSite,
}

impl From<Unproven> for Response {
    fn from(unproven: Unproven) -> Self {
        match unproven {
            Unproven::Missing => required(),
            Unproven::CrossSite => cross_site(),
        }
    }
}

/// The hash of the session token a request carries: a bearer token, or else the session
/// cookie. The cookie counts only from the server's own pages; a request no page sent may use
/// it to read.
pub(crate) fn proof(method: &Method, headers: &HeaderMap) -> Result<[u8; 32], Unproven> {
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    if let Some(token) = bearer {
        return Ok(token_hash(token));
    }
    let token = session_cookie(headers).ok_or(Unproven::Missing)?;
    let reads = matches!(*method, Method::GET | Method::HEAD);
    if same_origin(headers).unwrap_or(reads) {
        Ok(token_hash(token))
    } else {
        Err(Unproven::CrossSite)
    }
}

fn session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|cookies| cookies.split(';'))
        .find_map(|cookie| {
            cookie
                .trim()
                .strip_prefix(SESSION_COOKIE)?
                .strip_prefix('=')
        })
}

/// Whether the page that sent a request is the server's own: its `Origin` is the host the
/// request was sent to, over HTTPS, or over HTTP on this machine. `None` when no page sent it.
pub(crate) fn same_origin(headers: &HeaderMap) -> Option<bool> {
    let origin = headers.get(ORIGIN)?;
    let text = |value: &HeaderValue| value.to_str().ok().map(str::to_ascii_lowercase);
    let own = text(origin)
        .zip(headers.get(HOST).and_then(text))
        .is_some_and(|(origin, host)| match origin.split_once("://") {
            Some(("https", sent_to)) => sent_to == host,
            Some(("http", sent_to)) => sent_to == host && this_machine(&host),
            _ => false,
        });
    Some(own)
}

/// Whether `host`, with or without a port, names this machine.
fn this_machine(host: &str) -> bool {
    let name = host
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|digit| digit.is_ascii_digit()))
        .map_or(host, |(name, _)| name);
    let address = name.trim_start_matches('[').trim_end_matches(']');
    name == "localhost"
        || address
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(name, value)| (name.parse().unwrap(), HeaderValue::from_static(value)))
            .collect()
    }

    #[test]
    fn a_page_is_the_servers_own_when_its_origin_is_the_host_asked() {
        let from = |origin, host| same_origin(&headers(&[("origin", origin), ("host", host)]));
        let own = Some(true);
        let other = Some(false);

        assert_eq!(from("https://weacon.example", "weacon.example"), own);
        assert_eq!(from("https://Weacon.Example", "weacon.example"), own);
        assert_eq!(
            from("https://weacon.example:8443", "weacon.example:8443"),
            own
        );
        assert_eq!(from("https://other.example", "weacon.example"), other);
        assert_eq!(from("https://weacon.example:8443", "weacon.example"), other);
        assert_eq!(
            from("https://weacon.example.other.example", "weacon.example"),
            other
        );
        assert_eq!(from("null", "weacon.example"), other);
        assert_eq!(from("weacon.example", "weacon.example"), other);
        assert_eq!(from("ftp://weacon.example", "weacon.example"), other);
        // Plain HTTP is anyone on the network, except on this machine.
        assert_eq!(from("http://weacon.example", "weacon.example"), other);
        assert_eq!(from("http://localhost:8080", "localhost:8080"), own);
        assert_eq!(from("http://localhost", "localhost"), own);
        assert_eq!(from("http://127.0.0.1:8080", "127.0.0.1:8080"), own);
        assert_eq!(from("http://[::1]:8080", "[::1]:8080"), own);
        assert_eq!(
            from("http://localhost.other.example", "localhost.other.example"),
            other
        );
        assert_eq!(from("http://10.0.0.1:8080", "10.0.0.1:8080"), other);
        assert_eq!(from("http://localhost:8080", "localhost:9090"), other);
        assert_eq!(
            same_origin(&headers(&[("origin", "https://weacon.example")])),
            other
        );
        assert_eq!(same_origin(&headers(&[("host", "weacon.example")])), None);
    }

    #[test]
    fn a_cookie_counts_from_the_servers_own_pages_and_a_bearer_token_from_anyone() {
        let cookie = ("cookie", "__Host-weacon_session=abc");
        let own = [
            cookie,
            ("origin", "https://h.example"),
            ("host", "h.example"),
        ];
        let other = [
            cookie,
            ("origin", "https://o.example"),
            ("host", "h.example"),
        ];
        let bearer = [
            ("authorization", "Bearer abc"),
            ("origin", "https://o.example"),
        ];
        let proven = |method: Method, pairs: &[(&'static str, &'static str)]| match proof(
            &method,
            &headers(pairs),
        ) {
            Ok(hash) => (hash == token_hash("abc")).then_some("proven").unwrap(),
            Err(Unproven::Missing) => "missing",
            Err(Unproven::CrossSite) => "cross-site",
        };

        assert_eq!(proven(Method::POST, &own), "proven");
        assert_eq!(proven(Method::GET, &own), "proven");
        assert_eq!(proven(Method::POST, &other), "cross-site");
        assert_eq!(proven(Method::GET, &other), "cross-site");
        // No page sent these, so the cookie may read but not change.
        assert_eq!(proven(Method::GET, &[cookie]), "proven");
        assert_eq!(proven(Method::HEAD, &[cookie]), "proven");
        assert_eq!(proven(Method::POST, &[cookie]), "cross-site");
        assert_eq!(proven(Method::DELETE, &[cookie]), "cross-site");
        assert_eq!(proven(Method::POST, &bearer), "proven");
        assert_eq!(
            proven(Method::POST, &[("authorization", "Basic abc")]),
            "missing"
        );
        assert_eq!(proven(Method::GET, &[]), "missing");
    }

    #[test]
    fn the_session_cookie_is_found_among_others() {
        let found = |pairs: &[(&'static str, &'static str)]| {
            session_cookie(&headers(pairs)).map(str::to_owned)
        };

        assert_eq!(
            found(&[("cookie", "__Host-weacon_session=abc")]).as_deref(),
            Some("abc")
        );
        assert_eq!(
            found(&[("cookie", "theme=dark; __Host-weacon_session=abc; x=1")]).as_deref(),
            Some("abc")
        );
        assert_eq!(found(&[("cookie", "__Host-weacon_session_old=abc")]), None);
        assert_eq!(found(&[("cookie", "x__Host-weacon_session=abc")]), None);
        assert_eq!(found(&[("cookie", "theme=dark")]), None);
        assert_eq!(found(&[]), None);
    }
}
