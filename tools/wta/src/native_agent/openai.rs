//! Dependency-free OpenAI-compatible chat completions client.
//!
//! Hand-rolled blocking HTTP POST to `{base_url}/chat/completions`, mirroring
//! the GET probe in `llm_provider` — we avoid pulling a full HTTP client (and
//! its TLS + async transitive crates) into the wta tree. Targets are local /
//! BYOK OpenAI-compatible endpoints over plain HTTP (Ollama, Foundry Local).
//! Non-streaming: one request, one full reply (L0). Streaming SSE is a later
//! step if perceived latency demands it.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Generous budget — a small local model on CPU can be slow to first/last token.
const TIMEOUT: Duration = Duration::from_secs(120);

/// One chat message. Owned strings keep call sites simple; this is not hot.
#[derive(serde::Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: &str) -> Self {
        Self {
            role: role.to_string(),
            content: content.to_string(),
        }
    }
}

/// POST a chat completion and return the assistant text, or a human-readable
/// error string on any failure (no endpoint, timeout, non-200, unparseable).
pub fn chat_completion(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<String, String> {
    let base = base_url.trim().trim_end_matches('/');
    let after_scheme = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))
        .ok_or_else(|| format!("unsupported base_url scheme: {base}"))?;
    let (authority, base_path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| "bad port".to_string())?),
        None => (authority, 80),
    };
    let path = format!("{}/chat/completions", base_path.trim_end_matches('/'));

    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    })
    .to_string();

    let addr = format!("{host}:{port}")
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .next()
        .ok_or_else(|| "no address".to_string())?;
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT).map_err(|e| e.to_string())?;
    stream.set_read_timeout(Some(TIMEOUT)).ok();
    stream.set_write_timeout(Some(TIMEOUT)).ok();

    let req = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAuthorization: Bearer {api_key}\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).map_err(|e| e.to_string())?;

    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| "malformed response".to_string())?;
    let headers = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body_bytes = &raw[split + 4..];
    let decoded = if headers.contains("transfer-encoding: chunked") {
        dechunk(body_bytes)
    } else {
        body_bytes.to_vec()
    };

    let json: serde_json::Value = serde_json::from_slice(&decoded)
        .map_err(|e| format!("parse: {e}"))?;
    json.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            let err = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("no content in response");
            err.to_string()
        })
}

/// Decode HTTP/1.1 chunked transfer-encoding. Best-effort (same as the GET probe).
fn dechunk(mut buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = match buf.windows(2).position(|w| w == b"\r\n") {
            Some(i) => i,
            None => break,
        };
        let size_hex = String::from_utf8_lossy(&buf[..line_end]);
        let size = usize::from_str_radix(size_hex.split(';').next().unwrap_or("").trim(), 16)
            .unwrap_or(0);
        if size == 0 {
            break;
        }
        let data_start = line_end + 2;
        let data_end = data_start + size;
        if data_end > buf.len() {
            out.extend_from_slice(&buf[data_start..]);
            break;
        }
        out.extend_from_slice(&buf[data_start..data_end]);
        buf = &buf[(data_end + 2).min(buf.len())..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_completion_rejects_bad_scheme() {
        let r = chat_completion("ftp://x", "k", "m", &[]);
        assert!(r.is_err());
    }

    #[test]
    fn no_endpoint_yields_error_not_panic() {
        // Port 1 has no listener — fast connect failure becomes an Err string.
        let r = chat_completion("http://127.0.0.1:1/v1", "k", "m", &[ChatMessage::new("user", "hi")]);
        assert!(r.is_err());
    }
}
