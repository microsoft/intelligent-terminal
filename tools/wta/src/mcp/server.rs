//! Tokio-only HTTP/1.1 + JSON-RPC handler for the MCP endpoint. Single POST
//! endpoint, single-object `application/json` responses (no SSE), bound to
//! localhost only. Concurrency: one task per connection; the tool registry is
//! shared (`Arc`) and stateless.

use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::Tool;

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
        if !method.eq_ignore_ascii_case("POST") || path != "/mcp" {
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
            Ok(req) => dispatch(&tools, &req).await,
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

/// Dispatch a JSON-RPC request. `None` for notifications (no `id`).
async fn dispatch(tools: &[Arc<dyn Tool>], req: &serde_json::Value) -> Option<serde_json::Value> {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    if id.is_none() {
        return None; // notification (e.g. notifications/initialized) — ack only
    }
    let id = id.unwrap();
    let result = match method {
        "initialize" => serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "wta", "version": env!("CARGO_PKG_VERSION") }
        }),
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
            let empty = serde_json::json!({});
            let args = req
                .get("params")
                .and_then(|p| p.get("arguments"))
                .unwrap_or(&empty);
            match tools.iter().find(|t| t.name() == name) {
                None => return Some(error_obj(id, -32602, &format!("unknown tool: {name}"))),
                Some(tool) => match tool.call(args).await {
                    Ok(text) => serde_json::json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false
                    }),
                    Err(msg) => serde_json::json!({
                        "content": [{ "type": "text", "text": msg }],
                        "isError": true
                    }),
                },
            }
        }
        other => return Some(error_obj(id, -32601, &format!("method not found: {other}"))),
    };
    Some(serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }))
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
    use crate::mcp::default_registry;

    #[tokio::test]
    async fn initialize_and_tools_list_and_call() {
        let tools = default_registry();
        let init = dispatch(
            &tools,
            &serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize"}),
        )
        .await
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);
        let list = dispatch(
            &tools,
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
        assert!(
            names.contains(&"resolve_command".to_string()),
            "got {names:?}"
        );
        // Notification → no response.
        assert!(dispatch(
            &tools,
            &serde_json::json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .is_none());
        // Unknown tool → error result.
        let bad = dispatch(&tools, &serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"nope","arguments":{}}})).await.unwrap();
        assert_eq!(bad["error"]["code"], -32602);
    }

    #[test]
    fn content_length_parses_case_insensitive() {
        assert_eq!(
            content_length(b"POST /mcp\r\nContent-Length: 42\r\n\r\n"),
            42
        );
        assert_eq!(content_length(b"x\r\ncontent-length:7\r\n\r\n"), 7);
    }
}
