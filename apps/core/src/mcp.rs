//! The MCP the core serves to the agents it starts, on 127.0.0.1. Each request carries its run's
//! token, and a tool answers only from the code of that run's folder.

pub(crate) mod grants;
pub(crate) mod tools;

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, header};
use hyper_util::rt::{TokioIo, TokioTimer};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{McpCall, code};

use grants::{Grant, Grants, Revoke};
use weacon_agents::{Launch, McpServer};
use weacon_protocol::{AgentKind, App};

use crate::chats::engine::Start;
use crate::daemon::ACCEPT_BACKOFF;

// So local processes, other users' included, can't hold the core's sockets.
const CONNECTIONS: usize = 64;
const HEADER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_BODY: usize = 64 * 1024;

/// The core's MCP, listening.
#[derive(Clone, Debug)]
pub(crate) struct Mcp {
    pub(crate) url: String,
    pub(crate) grants: Grants,
}

type Body = BoxBody<Bytes, Infallible>;

/// Listens on a free port of 127.0.0.1 and serves until the returned task is aborted; must run
/// inside a tokio runtime.
pub(crate) fn start(app: &App, log: &Logger) -> Result<(Mcp, Server), Fault> {
    let failed = |error| {
        Fault::new(code!("mcp.start_failed"), "The local MCP couldn't start").because(error)
    };
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(failed)?;
    listener.set_nonblocking(true).map_err(failed)?;
    let listener = TcpListener::from_std(listener).map_err(failed)?;
    let address = listener.local_addr().map_err(failed)?;
    let grants = Grants::default();
    let log = log.child(&[("component", "mcp".into())]);
    let handler = Handler {
        version: app.version.clone(),
        log,
    };
    handler
        .log
        .info("mcp started", &[("port", address.port().into())]);
    let server = tokio::spawn(serve(listener, address, grants.clone(), handler));
    let mcp = Mcp {
        url: format!("http://{address}/mcp"),
        grants,
    };
    Ok((mcp, Server(server)))
}

/// The server's task, which stops when this is dropped.
#[derive(Debug)]
pub(crate) struct Server(JoinHandle<()>);

impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Points a Claude Code `launch` at the MCP with a token for `start`'s run, which works until
/// the [`Revoke`] is dropped. Other agents are left as they are.
pub(crate) fn grant(
    mcp: Option<&Mcp>,
    launch: Launch,
    start: &Start,
) -> Result<(Launch, Option<Revoke>), Fault> {
    let Some(mcp) = mcp.filter(|_| launch.agent == AgentKind::Claude) else {
        return Ok((launch, None));
    };
    let grant = Grant::new(
        start.workspace.clone(),
        start.run.clone(),
        launch.cwd.clone(),
    );
    let (token, revoke) = mcp.grants.issue(grant)?;
    let server = McpServer {
        url: mcp.url.clone(),
        token: token.as_str().to_owned(),
        tools: tools::listed()
            .into_iter()
            .map(|tool| tool.name.into_owned())
            .collect(),
    };
    Ok((
        Launch {
            mcp: Some(server),
            ..launch
        },
        Some(revoke),
    ))
}

async fn serve(listener: TcpListener, address: SocketAddr, grants: Grants, handler: Handler) {
    // Stateless: each request stands alone, so no session outlives its run's token. No origin is
    // allowed, so any request that names one is refused.
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts([address.to_string()])
        .enforce_origin_validation()
        .with_max_request_body_bytes(MAX_BODY);
    let log = handler.log.clone();
    let rmcp = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let front = Arc::new(Front { grants, rmcp, log });
    let open = Arc::new(Semaphore::new(CONNECTIONS));
    loop {
        let Ok(slot) = Arc::clone(&open).acquire_owned().await else {
            return;
        };
        match listener.accept().await {
            Ok((stream, _)) => {
                tokio::spawn(connection(stream, Arc::clone(&front), slot));
            }
            Err(error) => {
                // Such as too many open files: wait, rather than spin and flood the log.
                front
                    .log
                    .warn("mcp accept failed", &[("err", Field::error(&error))]);
                tokio::time::sleep(ACCEPT_BACKOFF).await;
            }
        }
    }
}

async fn connection(stream: TcpStream, front: Arc<Front>, _slot: OwnedSemaphorePermit) {
    let warned = Arc::new(AtomicBool::new(false));
    let service = service_fn(|request| {
        let (front, warned) = (Arc::clone(&front), Arc::clone(&warned));
        async move { Ok::<_, Infallible>(front.answer(request, &warned).await) }
    });
    // A client that hangs up mid-request, or never sends one, is no fault of the core's.
    drop(
        http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(HEADER_TIMEOUT)
            .serve_connection(TokioIo::new(stream), service)
            .await,
    );
}

struct Front {
    grants: Grants,
    rmcp: StreamableHttpService<Handler, LocalSessionManager>,
    log: Logger,
}

impl Front {
    /// Answers one request; a connection's refusals are logged only once, so none floods the log.
    async fn answer(&self, mut request: Request<Incoming>, warned: &AtomicBool) -> Response<Body> {
        if request.uri().path() != "/mcp" {
            return empty(StatusCode::NOT_FOUND);
        }
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let Some(grant) = token.and_then(|token| self.grants.find(token)) else {
            let reason = if token.is_some() {
                "unknown"
            } else {
                "missing"
            };
            if !warned.swap(true, Ordering::Relaxed) {
                self.log.warn("mcp refused", &[("reason", reason.into())]);
            }
            return empty(StatusCode::UNAUTHORIZED);
        };
        request.extensions_mut().insert(grant);
        self.rmcp.handle(request).await
    }
}

fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Empty::new().boxed());
    *response.status_mut() = status;
    response
}

#[derive(Clone)]
struct Handler {
    version: String,
    log: Logger,
}

impl ServerHandler for Handler {
    fn get_info(&self) -> InitializeResult {
        let capabilities = ServerCapabilities::builder().enable_tools().build();
        InitializeResult::new(capabilities)
            .with_server_info(Implementation::new("weacon", &self.version))
    }

    async fn list_tools(
        &self,
        _: Option<PaginatedRequestParams>,
        _: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools::listed()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let grant = context
            .extensions
            .get::<hyper::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<Arc<Grant>>())
            .cloned()
            .ok_or_else(|| ErrorData::invalid_request("No run", None))?;
        let call = McpCall::parse(&request.name, request.arguments)
            .map_err(|_| ErrorData::invalid_params("These arguments don't fit the tool", None))?;
        let tool = match &call {
            McpCall::Map(_) => "map",
            McpCall::Symbol(_) => "symbol",
            McpCall::Newer => return Err(ErrorData::invalid_params("No such tool", None)),
        };
        let Ok(_permit) = grant.calls.try_acquire() else {
            let busy = ContentBlock::text("Too many calls at once: wait for one to end.");
            return Ok(CallToolResult::error(vec![busy]).into());
        };
        let started = Instant::now();
        let folder = grant.folder.clone();
        let log = self.log.child(&[("run_id", grant.run.as_str().into())]);
        let text = tokio::task::spawn_blocking(move || tools::answer(&call, &folder, &log))
            .await
            .map_err(|_| ErrorData::internal_error("The tool failed", None))?;
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let fields: [(&str, Field); 5] = [
            ("workspace_id", grant.workspace.as_str().into()),
            ("run_id", grant.run.as_str().into()),
            ("tool", tool.into()),
            ("bytes", text.len().into()),
            ("ms", millis.into()),
        ];
        self.log.info("mcp answered", &fields);
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::path::PathBuf;

    use rmcp::model::{
        CallToolRequest, ClientCapabilities, ClientJsonRpcMessage, ClientRequest,
        InitializeRequest, InitializeRequestParams, JsonObject, JsonRpcRequest, ListToolsRequest,
        NumberOrString,
    };
    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use weacon_protocol::{RunId, WorkspaceId};
    use weacon_testing::{TempDir, recording_logger};

    use super::*;
    use crate::test_support::VERSION;

    struct Reply {
        status: u16,
        body: String,
    }

    fn rpc(request: ClientRequest) -> String {
        let message =
            ClientJsonRpcMessage::Request(JsonRpcRequest::new(NumberOrString::Number(1), request));
        serde_json::to_string(&message).unwrap()
    }

    fn call(name: &str, arguments: Value) -> String {
        let arguments: JsonObject = serde_json::from_value(arguments).unwrap();
        let params = CallToolRequestParams::new(name.to_owned()).with_arguments(arguments);
        rpc(ClientRequest::CallToolRequest(CallToolRequest::new(params)))
    }

    async fn post(mcp: &Mcp, headers: &[(&str, &str)], body: &str) -> Reply {
        let address = mcp
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp");
        let mut stream = TcpStream::connect(address).await.unwrap();
        let mut request = format!(
            "POST /mcp HTTP/1.1\r\nContent-Type: application/json\r\n\
             Accept: application/json, text/event-stream\r\nConnection: close\r\n\
             Content-Length: {}\r\n",
            body.len()
        );
        if !headers.iter().any(|(name, _)| *name == "Host") {
            write!(request, "Host: {address}\r\n").unwrap();
        }
        for (name, value) in headers {
            write!(request, "{name}: {value}\r\n").unwrap();
        }
        request.push_str("\r\n");
        request.push_str(body);
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();
        let (head, body) = reply.split_once("\r\n\r\n").unwrap();
        let status = head.split(' ').nth(1).unwrap().parse().unwrap();
        Reply {
            status,
            body: body.to_owned(),
        }
    }

    fn answer_text(reply: &Reply) -> String {
        let message: Value = serde_json::from_str(&reply.body).unwrap();
        let text = &message["result"]["content"][0]["text"];
        text.as_str().unwrap().to_owned()
    }

    fn repo(dir: &TempDir, name: &str) -> PathBuf {
        let folder = dir.join(name);
        std::fs::create_dir_all(folder.join("src")).unwrap();
        let manifest = format!("[package]\nname = \"{name}\"\n");
        std::fs::write(folder.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(folder.join("src/lib.rs"), "pub fn run() {}\n").unwrap();
        folder
    }

    fn run_grant(workspace: &str, run: &str, folder: PathBuf) -> Grant {
        Grant::new(
            WorkspaceId::try_from(workspace.to_owned()).unwrap(),
            RunId::try_from(run.to_owned()).unwrap(),
            folder,
        )
    }

    fn bearer(token: &grants::Token) -> String {
        format!("Bearer {}", token.as_str())
    }

    #[tokio::test]
    async fn each_token_reads_only_its_own_runs_folder() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let (first, _a) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        let (second, _b) = mcp
            .grants
            .issue(run_grant("w-2", "run-2", repo(&dir, "two")))
            .unwrap();
        let map = call("map", json!({}));

        let one = post(&mcp, &[("Authorization", &bearer(&first))], &map).await;
        let two = post(&mcp, &[("Authorization", &bearer(&second))], &map).await;

        assert_eq!((one.status, two.status), (200, 200));
        assert_eq!(
            answer_text(&one),
            "one .: 1 module\n  (root) src/lib.rs: 1 item\n"
        );
        assert_eq!(
            answer_text(&two),
            "two .: 1 module\n  (root) src/lib.rs: 1 item\n"
        );
    }

    #[tokio::test]
    async fn a_missing_unknown_or_ended_token_is_refused_and_never_logged() {
        let dir = TempDir::new().unwrap();
        let (log, records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let (ended, revoke) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        drop(revoke);
        let map = call("map", json!({}));

        let missing = post(&mcp, &[], &map).await;
        let unknown = post(&mcp, &[("Authorization", "Bearer marker-token-1")], &map).await;
        let gone = post(&mcp, &[("Authorization", &bearer(&ended))], &map).await;
        let basic = post(&mcp, &[("Authorization", "Basic marker-token-2")], &map).await;

        for reply in [&missing, &unknown, &gone, &basic] {
            assert_eq!((reply.status, reply.body.as_str()), (401, ""));
        }
        let logged = format!("{:?}", records.lock().unwrap());
        assert_eq!(logged.matches("mcp refused").count(), 4);
        for marker in ["marker-token", ended.as_str()] {
            assert!(!logged.contains(marker), "{marker}");
        }
    }

    #[tokio::test]
    async fn another_host_an_origin_or_a_large_body_is_refused() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let (token, _revoke) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        let auth = bearer(&token);
        let map = call("map", json!({}));
        let large = call("symbol", json!({"path": "x".repeat(MAX_BODY)}));
        let fits = call("symbol", json!({"path": "x".repeat(60_000)}));

        let host = post(
            &mcp,
            &[("Authorization", &auth), ("Host", "evil.test")],
            &map,
        )
        .await;
        let origin = [
            ("Authorization", auth.as_str()),
            ("Origin", "http://127.0.0.1"),
        ];
        let origin = post(&mcp, &origin, &map).await;
        let too_large = post(&mcp, &[("Authorization", &auth)], &large).await;
        let large_enough = post(&mcp, &[("Authorization", &auth)], &fits).await;

        assert_eq!(host.status, 403);
        assert_eq!(origin.status, 403);
        assert_eq!(too_large.status, 413);
        assert_eq!(answer_text(&large_enough), "No item has this path.\n");
    }

    #[tokio::test]
    async fn a_run_gets_only_so_many_calls_at_once_and_bad_arguments_are_refused() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let (token, _revoke) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        let auth = bearer(&token);
        let held = mcp.grants.find(token.as_str()).unwrap();
        let all = u32::try_from(grants::CALLS_AT_ONCE).unwrap();
        let permits = held.calls.acquire_many(all).await.unwrap();

        let busy = post(&mcp, &[("Authorization", &auth)], &call("map", json!({}))).await;
        drop(permits);
        let wrong = post(
            &mcp,
            &[("Authorization", &auth)],
            &call("symbol", json!({})),
        )
        .await;

        assert_eq!(
            answer_text(&busy),
            "Too many calls at once: wait for one to end."
        );
        let error: Value = serde_json::from_str(&wrong.body).unwrap();
        assert_eq!(
            error["error"]["message"],
            "These arguments don't fit the tool"
        );
        let later = call("later", json!({}));
        let unknown = post(&mcp, &[("Authorization", &auth)], &later).await;
        let error: Value = serde_json::from_str(&unknown.body).unwrap();
        assert_eq!(error["error"]["message"], "No such tool");
    }

    #[tokio::test]
    async fn it_names_itself_and_lists_its_tools() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let (token, _revoke) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        let auth = [("Authorization", bearer(&token))];
        let auth: Vec<_> = auth
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect();
        let client = Implementation::new("test", "1");
        let params = InitializeRequestParams::new(ClientCapabilities::default(), client);
        let hello = rpc(ClientRequest::InitializeRequest(InitializeRequest::new(
            params,
        )));
        let list = rpc(ClientRequest::ListToolsRequest(ListToolsRequest::default()));

        let hello: Value = serde_json::from_str(&post(&mcp, &auth, &hello).await.body).unwrap();
        let list: Value = serde_json::from_str(&post(&mcp, &auth, &list).await.body).unwrap();

        let info = &hello["result"]["serverInfo"];
        assert_eq!(
            (&info["name"], &info["version"]),
            (&json!("weacon"), &json!(VERSION.version))
        );
        assert!(hello["result"]["capabilities"]["tools"].is_object());
        let tools = serde_json::to_value(tools::listed()).unwrap();
        assert_eq!(list["result"]["tools"], tools);
    }

    #[tokio::test]
    async fn dropping_the_server_stops_it() {
        let (log, _records) = recording_logger();
        let (mcp, server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let address = mcp
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp");

        drop(server);
        tokio::task::yield_now().await;

        TcpStream::connect(address).await.unwrap_err();
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_that_sends_nothing_is_closed() {
        let (log, _records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let address = mcp
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp");
        let mut idle = TcpStream::connect(address).await.unwrap();

        let mut read = Vec::new();
        let closed = tokio::time::timeout(HEADER_TIMEOUT * 2, idle.read_to_end(&mut read)).await;

        closed.unwrap().unwrap();
    }

    #[tokio::test]
    async fn a_connection_is_warned_about_once_however_often_it_is_refused() {
        let (log, records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log).unwrap();
        let address = mcp
            .url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp");
        let body = call("map", json!({}));
        let request = |close: &str| {
            format!(
                "POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: {}\r\n{close}\r\n{body}",
                body.len()
            )
        };
        let mut stream = TcpStream::connect(address).await.unwrap();

        let both = request("") + &request("Connection: close\r\n");
        stream.write_all(both.as_bytes()).await.unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await.unwrap();

        assert_eq!(reply.matches("401 Unauthorized").count(), 2);
        assert_eq!(
            weacon_testing::messages(&records),
            ["mcp started", "mcp refused"]
        );
    }

    #[test]
    fn only_claude_gets_the_mcp_and_its_token_ends_with_the_revoke() {
        let mcp = Mcp {
            url: "http://127.0.0.1:9/mcp".into(),
            grants: Grants::default(),
        };
        let launch = |agent| Launch {
            agent,
            program: PathBuf::from("agent"),
            config_dir: PathBuf::from("/state/w/agent"),
            cwd: PathBuf::from("/work"),
            access: weacon_protocol::AccessLevel::AskFirst,
            mcp: None,
        };
        let start = Start {
            workspace: WorkspaceId::try_from("w-1".to_owned()).unwrap(),
            chat: weacon_protocol::ChatId::try_from("c-1".to_owned()).unwrap(),
            agent: AgentKind::Claude,
            run: RunId::try_from("run-1".to_owned()).unwrap(),
            session: None,
        };

        let (claude, revoke) = grant(Some(&mcp), launch(AgentKind::Claude), &start).unwrap();
        let (codex, none) = grant(Some(&mcp), launch(AgentKind::Codex), &start).unwrap();
        let (alone, no_mcp) = grant(None, launch(AgentKind::Claude), &start).unwrap();

        let server = claude.mcp.unwrap();
        assert_eq!(server.url, mcp.url);
        let found = mcp.grants.find(&server.token).unwrap();
        assert_eq!(
            (found.run.as_str(), found.folder.as_path()),
            ("run-1", "/work".as_ref())
        );
        assert_eq!((codex.mcp, none.is_none()), (None, true));
        assert_eq!((alone.mcp, no_mcp.is_none()), (None, true));
        drop(revoke);
        assert!(mcp.grants.find(&server.token).is_none());
    }
}
