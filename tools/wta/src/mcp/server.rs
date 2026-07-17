//! Tokio-only HTTP/1.1 + JSON-RPC handler for the MCP endpoint. Single POST
//! endpoint, single-object `application/json` responses (no SSE), bound to
//! localhost only. Concurrency: one task per connection; the tool registry is
//! shared (`Arc`) and stateless.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::{Tool, ToolContext};

const PROTOCOL_VERSION: &str = "2025-06-18";

/// Resolved endpoint of a running MCP server.
pub struct McpEndpoint {
    pub port: u16,
    pub url: String,
}

/// Bind a localhost MCP server on an ephemeral port and serve it on a
/// background task for the process lifetime. Returns the endpoint (URL to hand
/// to the agent via `McpServer::Http`). Bind failure returns `None` — callers
/// degrade to the in-process path, MCP just isn't offered.
pub async fn serve(tools: Vec<Arc<dyn Tool>>) -> Option<McpEndpoint> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.ok()?;
    let port = listener.local_addr().ok()?.port();
    let url = format!("http://127.0.0.1:{port}/mcp");
    let tools = Arc::new(tools);
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let tools = tools.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_conn(stream, tools).await {
                            tracing::debug!(target: "mcp", error = %e, "connection ended");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!(target: "mcp", error = %e, "accept failed");
                    break;
                }
            }
        }
    });
    tracing::info!(target: "mcp", url = %url, "MCP server listening");
    Some(McpEndpoint { port, url })
}

/// Read HTTP/1.1 POST requests (keep-alive) on one connection, dispatch the
/// JSON-RPC body, write back a JSON response.
async fn handle_conn(mut stream: TcpStream, tools: Arc<Vec<Arc<dyn Tool>>>) -> std::io::Result<()> {
    // Caps to keep a misbehaving/hostile local peer from forcing unbounded
    // memory growth: headers and body are both bounded; over-cap → close.
    const MAX_HEADERS: usize = 16 * 1024;
    const MAX_BODY: usize = 1024 * 1024;
    // Inactivity timeout: drop a peer that opens a connection but never finishes
    // sending headers/body (slowloris-style local resource exhaustion).
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    let mut buf = Vec::with_capacity(8192);
    loop {
        // Read until headers complete, then the declared body.
        let (headers_end, content_len) = loop {
            if let Some(end) = find_headers_end(&buf) {
                break (end, content_length(&buf[..end]));
            }
            if buf.len() > MAX_HEADERS {
                return Ok(()); // headers too large — drop
            }
            let mut chunk = [0u8; 4096];
            let n = match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await {
                Ok(r) => r?,
                Err(_) => return Ok(()), // idle too long — drop
            };
            if n == 0 {
                return Ok(()); // client closed
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        if content_len > MAX_BODY {
            return Ok(()); // body too large — drop
        }
        // Enforce POST /mcp: this is a single JSON-RPC endpoint. Anything else
        // → 405, don't treat the payload as a body.
        let mut request_line = std::str::from_utf8(&buf[..headers_end])
            .ok()
            .and_then(|s| s.lines().next())
            .unwrap_or("")
            .split_whitespace();
        let method = request_line.next().unwrap_or("");
        let path = request_line.next().unwrap_or("");
        let route_id = match mcp_route_id(path) {
            Some(route_id) => route_id.map(str::to_string),
            None => {
                write_json(&mut stream, 405, "").await?;
                return Ok(());
            }
        };
        if !method.eq_ignore_ascii_case("POST") {
            write_json(&mut stream, 405, "").await?;
            return Ok(());
        }
        let body_start = headers_end;
        while buf.len() < body_start + content_len {
            let mut chunk = [0u8; 4096];
            let n = match tokio::time::timeout(READ_TIMEOUT, stream.read(&mut chunk)).await {
                Ok(r) => r?,
                Err(_) => return Ok(()),
            };
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let body = buf[body_start..body_start + content_len].to_vec();
        buf.drain(..body_start + content_len);

        let resp = match serde_json::from_slice::<serde_json::Value>(&body) {
            Ok(req) => dispatch(&tools, route_id.as_deref(), &req).await,
            Err(_) => Some(error_obj(serde_json::Value::Null, -32700, "parse error")),
        };
        match resp {
            // Request → 200 with the JSON-RPC object.
            Some(v) => write_json(&mut stream, 200, &v.to_string()).await?,
            // Notification → 202 Accepted, no body (per Streamable HTTP).
            None => write_json(&mut stream, 202, "").await?,
        }
    }
}

/// In-flight `tools/call` tasks by request id, so `notifications/cancelled` can
/// abort a running tool (spec: the client owns timeouts and cancels; the server
/// honors it). Short-lived; entries are removed when the call finishes.
static CANCELS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>>,
> = std::sync::OnceLock::new();

fn cancels(
) -> &'static std::sync::Mutex<std::collections::HashMap<String, tokio::task::AbortHandle>> {
    CANCELS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn cancellation_key(route_id: Option<&str>, request_id: &serde_json::Value) -> String {
    format!("{}:{}", route_id.unwrap_or_default(), request_id)
}

/// Dispatch a JSON-RPC request. `None` for notifications (no `id`).
async fn dispatch(
    tools: &[Arc<dyn Tool>],
    route_id: Option<&str>,
    req: &serde_json::Value,
) -> Option<serde_json::Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    // Cancellation is a notification — abort the in-flight tool call, ack only.
    if method == "notifications/cancelled" {
        if let Some(rid) = req.get("params").and_then(|p| p.get("requestId")) {
            let key = cancellation_key(route_id, rid);
            if let Some(h) = cancels()
                .lock()
                .ok()
                .and_then(|m| m.get(&key).cloned())
            {
                h.abort();
            }
        }
        return None;
    }
    if id.is_none() {
        return None; // notification (e.g. notifications/initialized) — ack only
    }
    let id = id.unwrap();
    let result = match method {
        "initialize" => {
            // Echo the client's requested protocolVersion when present (spec
            // negotiation), else advertise our own (`PROTOCOL_VERSION`).
            let version = req
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION);
            serde_json::json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "wta", "version": env!("CARGO_PKG_VERSION") }
            })
        }
        "ping" => serde_json::json!({}),
        "tools/list" => serde_json::json!({
            "tools": tools.iter().map(|t| serde_json::json!({
                "name": t.name(),
                "description": t.description(),
                "inputSchema": t.input_schema(),
            })).collect::<Vec<_>>()
        }),
        "tools/call" => {
            let name = req
                .get("params")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("");
            let args = req
                .get("params")
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            match tools.iter().find(|t| t.name() == name) {
                None => return Some(error_obj(id, -32602, &format!("unknown tool: {name}"))),
                Some(tool) => {
                    // Run abortable so notifications/cancelled can stop it.
                    let key = cancellation_key(route_id, &id);
                    let tool = tool.clone();
                    let route_id = route_id.map(str::to_string);
                    let jh = tokio::spawn(async move {
                        tool.call(
                            &ToolContext {
                                route_id: route_id.as_deref(),
                            },
                            &args,
                        )
                        .await
                    });
                    cancels()
                        .lock()
                        .unwrap()
                        .insert(key.clone(), jh.abort_handle());
                    let r = jh.await;
                    cancels().lock().unwrap().remove(&key);
                    match r {
                        Ok(Ok(text)) => {
                            serde_json::json!({ "content": [{ "type": "text", "text": text }], "isError": false })
                        }
                        Ok(Err(msg)) => {
                            serde_json::json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
                        }
                        Err(_) => return None, // cancelled — client already moved on
                    }
                }
            }
        }
        other => return Some(error_obj(id, -32601, &format!("method not found: {other}"))),
    };
    Some(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn mcp_route_id(path: &str) -> Option<Option<&str>> {
    if path == "/mcp" {
        return Some(None);
    }
    let route = path.strip_prefix("/mcp/")?;
    if route.is_empty()
        || route.len() > 64
        || !route.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    Some(Some(route))
}

fn error_obj(id: serde_json::Value, code: i32, msg: &str) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn content_length(headers: &[u8]) -> usize {
    std::str::from_utf8(headers)
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| {
                    l.split_once(':')
                        .filter(|(k, _)| k.trim().eq_ignore_ascii_case("content-length"))
                })
                .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        })
        .unwrap_or(0)
}

async fn write_json(stream: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        405 => "Method Not Allowed",
        _ => "OK",
    };
    let resp = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(resp.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::{default_registry, RouteRegistry};

    #[tokio::test]
    async fn initialize_and_tools_list_and_call() {
        let tools = default_registry(RouteRegistry::default());
        let init = dispatch(
            &tools,
            None,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        // Echoes the client's requested version (negotiation).
        let echo = dispatch(&tools, None, &serde_json::json!({"jsonrpc":"2.0","id":9,"method":"initialize","params":{"protocolVersion":"2024-11-05"}})).await.unwrap();
        assert_eq!(echo["result"]["protocolVersion"], "2024-11-05");
        let list = dispatch(
            &tools,
            None,
            &serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await
        .unwrap();
        let names: Vec<_> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            ["resolve_command", "propose_terminal_actions"],
            "the unified proposal protocol must expose one action tool"
        );
        // Notification → no response.
        assert!(dispatch(
            &tools,
            None,
            &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .is_none());
        // Unknown tool → error result.
        let bad = dispatch(&tools, None, &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope","arguments":{}}})).await.unwrap();
        assert_eq!(bad["error"]["code"], -32602);
    }

    #[test]
    fn content_length_parses_case_insensitive() {
        assert_eq!(
            content_length(b"POST /mcp\r\nContent-Length: 42\r\n\r\n"),
            42
        );
        assert_eq!(content_length(b"x\r\ncontent-length:7\r\n\r\n"), 7);
        assert_eq!(content_length(b"no length header\r\n\r\n"), 0);
    }

    #[test]
    fn finds_header_terminator() {
        assert_eq!(find_headers_end(b"POST /mcp\r\n\r\nbody"), Some(13));
        assert_eq!(find_headers_end(b"incomplete\r\n"), None);
    }

    #[tokio::test]
    async fn unknown_method_is_32601() {
        let tools = default_registry(RouteRegistry::default());
        let r = dispatch(
            &tools,
            None,
            &serde_json::json!({"jsonrpc":"2.0","id":7,"method":"frobnicate"}),
        )
        .await
        .unwrap();
        assert_eq!(r["error"]["code"], -32601);
    }

    /// A tool that blocks until aborted, so we can prove notifications/cancelled
    /// stops an in-flight call.
    struct SleepTool;
    #[async_trait::async_trait]
    impl Tool for SleepTool {
        fn name(&self) -> &'static str {
            "sleep"
        }
        fn description(&self) -> &'static str {
            "blocks"
        }
        fn input_schema(&self) -> serde_json::Value {
            serde_json::json!({"type":"object"})
        }
        async fn call(&self, _context: &ToolContext<'_>, _a: &serde_json::Value) -> Result<String, String> {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
            Ok("never".into())
        }
    }

    #[tokio::test]
    async fn cancelled_aborts_in_flight_tool_call() {
        let tools: Vec<Arc<dyn Tool>> = vec![Arc::new(SleepTool)];
        let call = serde_json::json!({"jsonrpc":"2.0","id":"X","method":"tools/call","params":{"name":"sleep","arguments":{}}});
        let cancel = serde_json::json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"X"}});
        let t2 = tools.clone();
        let caller = tokio::spawn(async move { dispatch(&tools, None, &call).await });
        // Let the call register, then cancel by id.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(dispatch(&t2, None, &cancel).await.is_none());
        let res = tokio::time::timeout(std::time::Duration::from_secs(2), caller)
            .await
            .expect("call must end after cancel")
            .unwrap();
        assert!(res.is_none(), "cancelled call yields no response");
    }

    #[tokio::test]
    async fn serve_handles_post_and_rejects_non_post() {
        let ep = serve(default_registry(RouteRegistry::default())).await.unwrap();
        let post = |body: &str| {
            format!(
                "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
        };
        // tools/list over a real socket.
        let mut s = tokio::net::TcpStream::connect(("127.0.0.1", ep.port))
            .await
            .unwrap();
        s.write_all(post(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#).as_bytes())
            .await
            .unwrap();
        let mut buf = vec![0u8; 4096];
        let n = s.read(&mut buf).await.unwrap();
        let resp = String::from_utf8_lossy(&buf[..n]);
        assert!(resp.starts_with("HTTP/1.1 200"), "got {resp}");
        assert!(resp.contains("resolve_command"), "got {resp}");
        // Non-POST → 405.
        let mut s2 = tokio::net::TcpStream::connect(("127.0.0.1", ep.port))
            .await
            .unwrap();
        s2.write_all(b"GET /mcp HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let n2 = s2.read(&mut buf).await.unwrap();
        assert!(String::from_utf8_lossy(&buf[..n2]).starts_with("HTTP/1.1 405"));
    }

    #[test]
    fn route_path_accepts_base_and_opaque_route_only() {
        assert_eq!(mcp_route_id("/mcp"), Some(None));
        assert_eq!(mcp_route_id("/mcp/abc_123-X"), Some(Some("abc_123-X")));
        assert_eq!(mcp_route_id("/mcp/"), None);
        assert_eq!(mcp_route_id("/other"), None);
        assert_eq!(mcp_route_id("/mcp/a/b"), None);
    }

    #[test]
    fn cancellation_keys_are_route_scoped() {
        let request_id = serde_json::json!(1);
        assert_ne!(
            cancellation_key(Some("route-a"), &request_id),
            cancellation_key(Some("route-b"), &request_id)
        );
    }
}
