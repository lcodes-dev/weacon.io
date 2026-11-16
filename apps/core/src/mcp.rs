//! The MCP the core serves to the agents it starts, on 127.0.0.1. Each request carries its run's
//! token, and a tool answers only from the code of that run's folder.

pub(crate) mod grants;
mod maps;
mod propose;
pub(crate) mod tools;

use std::convert::Infallible;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Instant;

use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode, header};
use hyper_util::rt::TokioIo;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Extensions,
    Implementation, InitializeResult, ListToolsResult, PaginatedRequestParams, ServerCapabilities,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use weacon_observability::{Fault, Field, Logger};
use weacon_protocol::{McpCall, code};

use grants::{Grant, Grants, Revoke};
pub(crate) use maps::Maps;
#[cfg(test)]
pub(crate) use maps::test_support as maps_support;
use weacon_agents::{Launch, McpServer};
use weacon_protocol::App;

use crate::chats::engine::RunAs;
use crate::daemon::ACCEPT_BACKOFF;
use crate::proposals::Proposals;

/// Room for the largest proposal a run may send, and its envelope.
const MAX_BODY: usize = weacon_protocol::MAX_PROPOSAL + 64 * 1024;

/// The core's MCP, listening.
#[derive(Clone, Debug)]
pub(crate) struct Mcp {
    pub(crate) url: String,
    pub(crate) grants: Grants,
}

type Body = BoxBody<Bytes, Infallible>;

/// Listens on a free port of 127.0.0.1 and serves until the returned task is aborted, answering
/// from `maps` and keeping them current; must run inside a tokio runtime.
pub(crate) fn start(app: &App, log: &Logger, maps: Maps) -> Result<(Mcp, Server), Fault> {
    let failed = |error| {
        Fault::new(code!("mcp.start_failed"), "The local MCP couldn't start").because(error)
    };
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(failed)?;
    listener.set_nonblocking(true).map_err(failed)?;
    let listener = TcpListener::from_std(listener).map_err(failed)?;
    let address = listener.local_addr().map_err(failed)?;
    let grants = Grants::default();
    let log = log.child(&[("component", "mcp".into())]);
    let (watched, watch_log, watched_grants) = (maps.clone(), log.clone(), grants.clone());
    let handler = Handler {
        version: app.version.clone(),
        proposals: maps.proposals(),
        maps,
        log,
    };
    handler
        .log
        .info("mcp started", &[("port", address.port().into())]);
    let serving = serve(listener, address, grants.clone(), handler);
    // One task, so stopping the server stops the watches too.
    let server = tokio::spawn(async move {
        let keep_current = watched.keep_current(&watched_grants, &watch_log);
        tokio::join!(serving, keep_current);
    });
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

/// Points `launch`, if its agent takes the MCP, at it with a token for `run_as`'s run, which
/// works until the [`Revoke`] is dropped.
pub(crate) fn grant(
    mcp: Option<&Mcp>,
    launch: Launch,
    run_as: &RunAs,
) -> Result<(Launch, Option<Revoke>), Fault> {
    let Some(mcp) = mcp.filter(|_| launch.takes_mcp()) else {
        return Ok((launch, None));
    };
    let grant = Grant::new(
        run_as.key.workspace.clone(),
        run_as.run.clone(),
        run_as.agent,
        launch.cwd.clone(),
    )
    .for_task(run_as.task.clone());
    let (token, revoke) = mcp.grants.issue(grant)?;
    let server = McpServer {
        url: mcp.url.clone(),
        token: token.as_str().to_owned(),
        tools: tools::UP_FRONT.map(str::to_owned).to_vec(),
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
        .enforce_origin_validation();
    let log = handler.log.clone();
    let rmcp = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let front = Arc::new(Front { grants, rmcp, log });
    accept(&listener, &front).await;
}

async fn accept(listener: &TcpListener, front: &Arc<Front>) {
    // Owned here, so stopping the server ends its connections too.
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            Some(_) = connections.join_next() => {}
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    connections.spawn(connection(stream, Arc::clone(front)));
                }
                Err(error) => {
                    // Such as too many open files: wait, rather than spin and flood the log.
                    front.log.warn("mcp accept failed", &[("err", Field::error(&error))]);
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                }
            },
        }
    }
}

async fn connection(stream: TcpStream, front: Arc<Front>) {
    let service = service_fn(|request| {
        let front = Arc::clone(&front);
        async move { Ok::<_, Infallible>(front.answer(request).await) }
    });
    // A client that hangs up mid-request, or never sends one, is no fault of the core's.
    drop(
        http1::Builder::new()
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
    async fn answer(&self, mut request: Request<Incoming>) -> Response<Body> {
        if request.uri().path() != "/mcp" {
            return empty(StatusCode::NOT_FOUND);
        }
        let token = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        let missing = || Fault::new(code!("mcp.token_missing"), "The request has no token");
        let unknown = || Fault::new(code!("mcp.token_unknown"), "The token isn't a live run's");
        let found = token
            .ok_or_else(missing)
            .and_then(|token| self.grants.find(token).ok_or_else(unknown));
        let grant = match found {
            Ok(grant) => grant,
            Err(fault) => {
                self.log
                    .warn("mcp refused", &[("err", Field::error(&fault))]);
                return empty(StatusCode::UNAUTHORIZED);
            }
        };
        request.extensions_mut().insert(grant);
        let (parts, body) = request.into_parts();
        match Limited::new(body, MAX_BODY).collect().await {
            Ok(body) => {
                let request = Request::from_parts(parts, Full::new(body.to_bytes()));
                self.rmcp.handle(request).await
            }
            Err(_) => empty(StatusCode::PAYLOAD_TOO_LARGE),
        }
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
    maps: Maps,
    /// Where `propose` sends; none when the core has no way to its server.
    proposals: Option<Proposals>,
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
        let grant = live_grant(&context.extensions)?;
        let call = McpCall::parse(&request.name, request.arguments)
            .map_err(|_| ErrorData::invalid_params("These arguments don't fit the tool", None))?;
        let Some(tool) = call.name() else {
            return Err(ErrorData::invalid_params("No such tool", None));
        };
        let started = Instant::now();
        let log = self.log.child(&[("run_id", grant.run.as_str().into())]);
        let answered = if let McpCall::Propose(args) = call {
            let repo = self.maps.repo(&grant, &log).await;
            let proposals = self.proposals.as_ref();
            propose::propose(proposals, &grant, args, repo.as_ref(), &log).await
        } else {
            Ok(self.maps.answer(&call, &grant, &log).await)
        };
        let text = answered
            .as_ref()
            .map_or_else(String::as_str, String::as_str);
        let millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let fields: [(&str, Field); 5] = [
            ("workspace_id", grant.workspace.as_str().into()),
            ("run_id", grant.run.as_str().into()),
            ("tool", tool.into()),
            ("bytes", text.len().into()),
            ("ms", millis.into()),
        ];
        self.log.info("mcp answered", &fields);
        Ok(match answered {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(text) => CallToolResult::error(vec![ContentBlock::text(text)]),
        }
        .into())
    }
}

/// The grant the request was let in with, if its run is still alive: one can end while the
/// request's body is still being read.
fn live_grant(extensions: &Extensions) -> Result<Arc<Grant>, ErrorData> {
    extensions
        .get::<hyper::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<Arc<Grant>>())
        .filter(|grant| grant.is_live())
        .cloned()
        .ok_or_else(|| ErrorData::invalid_request("No run", None))
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::path::PathBuf;
    use std::time::Duration;

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
    use weacon_protocol::AgentKind;

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

    /// The `host:port` the MCP listens on.
    fn address(mcp: &Mcp) -> &str {
        mcp.url
            .trim_start_matches("http://")
            .trim_end_matches("/mcp")
    }

    async fn post(mcp: &Mcp, headers: &[(&str, &str)], body: &str) -> Reply {
        let address = address(mcp);
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
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_string(&mut reply));
        read.await.unwrap().unwrap();
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
            AgentKind::Claude,
            folder,
        )
    }

    fn bearer(token: &grants::Token) -> String {
        format!("Bearer {}", token.as_str())
    }

    #[tokio::test]
    async fn each_token_reads_only_its_own_runs_registered_repository() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let setup = maps::test_support::setup(&dir).await;
        let one = maps::test_support::workspace("one");
        let repo = maps::test_support::crate_repo(&dir, "one");
        setup.register(&one, "one", &repo);
        let build = weacon_appkit::build(&VERSION);
        let (mcp, _server) = start(&build, &log, setup.maps.clone()).unwrap();
        let (first, _a) = mcp
            .grants
            .issue(run_grant("one", "run-1", repo.clone()))
            .unwrap();
        // Another workspace's run in the repository only `one` registered.
        let (second, _b) = mcp
            .grants
            .issue(run_grant("two", "run-2", repo.clone()))
            .unwrap();
        let map = call("map", json!({}));

        let one = post(&mcp, &[("Authorization", &bearer(&first))], &map).await;
        let two = post(&mcp, &[("Authorization", &bearer(&second))], &map).await;

        assert_eq!((one.status, two.status), (200, 200));
        assert_eq!(
            answer_text(&one),
            "Start here: 1 crates, 1 items.\nEntry points:\nMost central items:\n\
             - fn one::run  src/lib.rs:1\nCrates:\n- one  .  1 items\n"
        );
        assert_eq!(answer_text(&two), tools::NO_MAP);
        assert!(
            !setup
                .stored(&maps::test_support::workspace("two"), &repo)
                .await
        );
    }

    #[tokio::test]
    async fn a_missing_unknown_or_ended_token_is_refused_and_never_logged() {
        let dir = TempDir::new().unwrap();
        let (log, records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log, Maps::off()).unwrap();
        let (ended, revoke) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        drop(revoke);
        let map = call("map", json!({}));

        // The one refusal logged is the one carrying a token.
        let unknown = post(&mcp, &[("Authorization", "Bearer marker-token-1")], &map).await;
        let missing = post(&mcp, &[], &map).await;
        let gone = post(&mcp, &[("Authorization", &bearer(&ended))], &map).await;
        let basic = post(&mcp, &[("Authorization", "Basic marker-token-2")], &map).await;

        for reply in [&missing, &unknown, &gone, &basic] {
            assert_eq!((reply.status, reply.body.as_str()), (401, ""));
        }
        let logged = format!("{:?}", records.lock().unwrap());
        assert_eq!(logged.matches("mcp refused").count(), 4);
        assert!(logged.contains("mcp.token_unknown"), "{logged}");
        for marker in ["marker-token", ended.as_str()] {
            assert!(!logged.contains(marker), "{marker}");
        }
    }

    #[tokio::test]
    async fn another_host_an_origin_or_a_large_body_is_refused() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log, Maps::off()).unwrap();
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
        assert_eq!(answer_text(&large_enough), tools::NO_MAP);
    }

    #[tokio::test]
    async fn bad_arguments_and_unknown_tools_are_refused() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log, Maps::off()).unwrap();
        let (token, _revoke) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        let auth = bearer(&token);
        let wrong = post(
            &mcp,
            &[("Authorization", &auth)],
            &call("symbol", json!({})),
        )
        .await;

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
        let (mcp, _server) = start(&weacon_appkit::build(&VERSION), &log, Maps::off()).unwrap();
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

    // Whether the server ended `stream`, with nothing more said.
    async fn ended(stream: &mut TcpStream) -> bool {
        let mut rest = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest))
            .await
            .is_ok_and(|read| read.map_or(true, |bytes| bytes == 0))
    }

    #[tokio::test]
    async fn dropping_the_server_stops_it_and_its_open_connections() {
        let (log, _records) = recording_logger();
        let (mcp, server) = start(&weacon_appkit::build(&VERSION), &log, Maps::off()).unwrap();
        let address = address(&mcp);
        let mut open = TcpStream::connect(address).await.unwrap();
        let request = format!("POST /mcp HTTP/1.1\r\nHost: {address}\r\nContent-Length: 0\r\n\r\n");
        open.write_all(request.as_bytes()).await.unwrap();
        let mut answered = Vec::new();
        while !answered.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            open.read_exact(&mut byte).await.unwrap();
            answered.extend(byte);
        }

        // Past the limit, this one waits unaccepted until the listener closes; a fresh connect
        // could reach another test's listener on the freed port.
        let mut queued = TcpStream::connect(address).await.unwrap();

        drop(server);

        assert!(String::from_utf8_lossy(&answered).starts_with("HTTP/1.1 401"));
        assert!(ended(&mut open).await);
        assert!(ended(&mut queued).await);
    }

    #[test]
    fn a_request_whose_run_ended_while_it_was_read_is_refused() {
        let grants = Grants::default();
        let (token, revoke) = grants
            .issue(run_grant("w-1", "run-1", PathBuf::from("/work")))
            .unwrap();
        let held = grants.find(token.as_str()).unwrap();
        let (parts, ()) = Request::builder()
            .extension(held)
            .body(())
            .unwrap()
            .into_parts();
        let mut extensions = Extensions::default();
        extensions.insert(parts);

        assert_eq!(live_grant(&extensions).unwrap().run.as_str(), "run-1");
        drop(revoke);
        assert_eq!(live_grant(&extensions).unwrap_err().message, "No run");
        live_grant(&Extensions::default()).unwrap_err();
    }

    #[test]
    fn only_claude_gets_the_mcp_with_the_up_front_tools_and_a_token_the_revoke_ends() {
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
            plugin: None,
        };
        let run_as = RunAs {
            key: crate::chats::test_support::key(),
            agent: AgentKind::Claude,
            run: RunId::try_from("run-1".to_owned()).unwrap(),
            session: None,
            repo: None,
            task: Some(weacon_protocol::ChatTask {
                id: "t-1".parse().unwrap(),
                title: "Speed up the build".to_owned(),
            }),
        };

        let (claude, revoke) = grant(Some(&mcp), launch(AgentKind::Claude), &run_as).unwrap();
        let (codex, none) = grant(Some(&mcp), launch(AgentKind::Codex), &run_as).unwrap();
        let (alone, no_mcp) = grant(None, launch(AgentKind::Claude), &run_as).unwrap();

        let server = claude.mcp.unwrap();
        assert_eq!(server.url, mcp.url);
        assert_eq!(server.tools, tools::UP_FRONT);
        let found = mcp.grants.find(&server.token).unwrap();
        assert_eq!(
            (found.run.as_str(), found.folder.as_path()),
            ("run-1", "/work".as_ref())
        );
        assert_eq!(found.task, run_as.task);
        assert_eq!((codex.mcp, none.is_none()), (None, true));
        assert_eq!((alone.mcp, no_mcp.is_none()), (None, true));
        drop(revoke);
        assert!(mcp.grants.find(&server.token).is_none());
    }

    #[tokio::test]
    async fn propose_answers_from_the_server_and_says_when_it_was_refused() {
        let dir = TempDir::new().unwrap();
        let (log, _records) = recording_logger();
        let fake = crate::proposals::fake::Fake::default();
        let maps = Maps::off().proposing(fake.connect());
        let build = weacon_appkit::build(&VERSION);
        let (mcp, _server) = start(&build, &log, maps).unwrap();
        let (token, _revoke) = mcp
            .grants
            .issue(run_grant("w-1", "run-1", repo(&dir, "one")))
            .unwrap();
        let auth = bearer(&token);
        let task = call(
            "propose",
            json!({"change": {"kind": "new_task", "title": "Ship"}}),
        );

        let proposed = post(&mcp, &[("Authorization", &auth)], &task).await;
        *fake.refuse.lock().unwrap() = Some(code!("proposal.invalid"));
        let refused = post(&mcp, &[("Authorization", &auth)], &task).await;

        let error = |reply: &Reply| {
            let message: Value = serde_json::from_str(&reply.body).unwrap();
            message["result"]["isError"].as_bool()
        };
        assert_eq!(answer_text(&proposed), propose::PROPOSED);
        assert_eq!(error(&proposed), Some(false));
        assert_eq!(answer_text(&refused), "Not proposed: proposal.invalid.\n");
        assert_eq!(error(&refused), Some(true));
        assert_eq!(fake.commands().len(), 2);
        assert_eq!(MAX_BODY, weacon_protocol::MAX_PROPOSAL + 65_536);
        // A doc's whole text is more than any other call sends.
        *fake.refuse.lock().unwrap() = None;
        let large = json!({"change": {"kind": "new_task", "title": "x".repeat(MAX_BODY / 2)}});
        let large = post(&mcp, &[("Authorization", &auth)], &call("propose", large)).await;
        assert_eq!((large.status, error(&large)), (200, Some(false)));
    }
}
