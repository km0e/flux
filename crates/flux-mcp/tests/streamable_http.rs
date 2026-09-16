//! Streamable HTTP client loopback test: a real rmcp server
//! (axum-mounted `StreamableHttpService`) on 127.0.0.1:0, connected
//! through `connect_with_peer`'s `McpServerConfig::Http` path — the same
//! code path the server's `McpManager` drives for http-kind rows. Covers
//! the initialize handshake, tool listing/wrapping, a wrapped tool CALL,
//! and that the configured custom headers reach the server.

// Hermeticity guard: proxy/`FLUX_*` env vars must never leak into the test
// process (the rmcp HTTP client would detour loopback through a host
// proxy). Shared via flux-test-support; runs pre-`main`.
flux_test_support::test_env_guard!();

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::http::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use flux_mcp::{McpServerConfig, connect_with_peer};
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, JsonObject,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{MaybeSendFuture, RequestContext, RoleServer};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
// `McpError` in the ServerHandler signatures is an alias for ErrorData.
use rmcp::model::ErrorData as McpError;

fn echo_schema() -> Arc<JsonObject> {
    Arc::new(
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        })
        .as_object()
        .cloned()
        .unwrap(),
    )
}

fn echo_tool() -> Tool {
    Tool::new("loopback_echo", "echoes its text argument", echo_schema())
}

/// A minimal MCP server exposing ONE tool: `loopback_echo`.
struct EchoServer;

impl ServerHandler for EchoServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        (name == "loopback_echo").then(echo_tool)
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, McpError>> + MaybeSendFuture + '_ {
        std::future::ready(Ok(ListToolsResult::with_all_items(vec![echo_tool()])))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let text = request
            .arguments
            .as_ref()
            .and_then(|a| a.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        Ok(CallToolResult::success(vec![ContentBlock::text(format!("echo: {text}"))]).into())
    }
}

async fn capture_header(
    seen: Arc<Mutex<Option<String>>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    if let Some(v) = req.headers().get("authorization") {
        *seen.lock().unwrap() = v.to_str().ok().map(str::to_string);
    }
    next.run(req).await
}

async fn spawn_http_mcp_server() -> (String, Arc<Mutex<Option<String>>>) {
    let seen: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let service = StreamableHttpService::new(
        || Ok(EchoServer),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );
    let seen_for_layer = Arc::clone(&seen);
    let app = Router::new()
        .route_service("/mcp", service)
        // The loopback's whole point: custom headers ride EVERY request.
        .layer(middleware::from_fn(move |req, next: Next| {
            let seen = Arc::clone(&seen_for_layer);
            capture_header(seen, req, next)
        }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, seen)
}

fn http_config(url: &str) -> McpServerConfig {
    McpServerConfig::Http {
        url: url.to_string(),
        headers: HashMap::from([("Authorization".to_string(), "Bearer loopback".to_string())]),
    }
}

fn text_of(result: CallToolResult) -> String {
    result
        .content
        .into_iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.text.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn http_connect_lists_tools_and_wraps_them() {
    let (url, seen) = spawn_http_mcp_server().await;

    let (mut session, _cancel, peer, tools) =
        connect_with_peer(&http_config(&url), None).await.unwrap();

    // The advertised tool arrived through the HTTP handshake.
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(names, vec!["loopback_echo"]);

    // A wrapped call round-trips over the SAME transport.
    let args: JsonObject = serde_json::json!({ "text": "hello over http" })
        .as_object()
        .cloned()
        .unwrap();
    let result = peer
        .call_tool(CallToolRequestParams::new("loopback_echo".to_string()).with_arguments(args))
        .await
        .unwrap();
    assert_eq!(text_of(result), "echo: hello over http");

    // The custom header reached the server.
    assert_eq!(
        seen.lock().unwrap().as_deref(),
        Some("Bearer loopback"),
        "Authorization header must ride the HTTP requests"
    );

    // Clean teardown: cancel ends the session (Cancelled, not a failure).
    session.cancel();
    assert!(matches!(session.quit().await, flux_mcp::McpQuit::Cancelled));
}

#[tokio::test]
async fn http_connect_to_a_dead_url_fails_cleanly() {
    // Bind and immediately drop the listener: nothing is listening there.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    drop(listener);

    let err = connect_with_peer(&http_config(&url), None)
        .await
        .map(|_| ())
        .unwrap_err();
    // The exact failure shape (refused vs the 30s timeout) is
    // timing-dependent; both are a clean McpError the supervisor treats as
    // a failed connect.
    let text = err.to_string();
    assert!(
        text.contains("MCP") || text.contains("timed out") || text.contains("error"),
        "unexpected error shape: {text}"
    );
}

#[tokio::test]
async fn http_connect_rejects_malformed_header_name() {
    let cfg = McpServerConfig::Http {
        url: "http://127.0.0.1:1/mcp".to_string(),
        headers: HashMap::from([("bad header name".to_string(), "v".to_string())]),
    };
    let err = connect_with_peer(&cfg, None).await.map(|_| ()).unwrap_err();
    assert!(err.to_string().contains("invalid MCP HTTP header"));
}
