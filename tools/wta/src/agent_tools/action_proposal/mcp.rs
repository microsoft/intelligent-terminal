use anyhow::{Context, Result};
use serde_json::{json, Value};
use tokio::io::{self, AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use super::channel::ProposalValidationStatus;
use super::pipe::{ProposalMcpPipeRequest, ProposalValidationResponse, PROTOCOL_VERSION};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_MCP_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", MCP_PROTOCOL_VERSION];
const TOOL_NAME: &str = "request_terminal_actions";
const MAX_MCP_MESSAGE_BYTES: usize = 1024 * 1024;
const HELPER_RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

pub async fn run_stdio(pipe_name: String, capability: String) -> Result<()> {
    let stdin = BufReader::new(io::stdin());
    let stdout = io::stdout();
    run(stdin, stdout, &pipe_name, &capability).await
}

async fn run<R, W>(mut reader: R, mut writer: W, pipe_name: &str, capability: &str) -> Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    while let Some(line) = read_message(&mut reader).await? {
        let request: Value = match serde_json::from_slice(&line) {
            Ok(request) => request,
            Err(_) => {
                write_json(
                    &mut writer,
                    error_response(Value::Null, -32700, "parse error"),
                )
                .await?;
                continue;
            }
        };
        if let Some(response) = dispatch(request, pipe_name, capability).await {
            write_json(&mut writer, response).await?;
        }
    }
    Ok(())
}

async fn read_message<R>(reader: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncBufRead + Unpin,
{
    let mut message = Vec::new();
    loop {
        let available = reader.fill_buf().await.context("read MCP stdio request")?;
        if available.is_empty() {
            if message.is_empty() {
                return Ok(None);
            }
            anyhow::bail!("MCP request is not newline terminated");
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if message.len() + consumed > MAX_MCP_MESSAGE_BYTES {
            anyhow::bail!("MCP request exceeds {MAX_MCP_MESSAGE_BYTES} bytes");
        }
        message.extend_from_slice(&available[..consumed]);
        reader.consume(consumed);
        if message.last() == Some(&b'\n') {
            return Ok(Some(message));
        }
    }
}

async fn dispatch(request: Value, pipe_name: &str, capability: &str) -> Option<Value> {
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");
    if id.is_none() {
        return None;
    }
    let id = id.unwrap();
    let result = match method {
        "initialize" => {
            let version = request
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .filter(|version| SUPPORTED_MCP_PROTOCOL_VERSIONS.contains(version))
                .unwrap_or(MCP_PROTOCOL_VERSION);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "intelligent-terminal",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": [{
                "name": TOOL_NAME,
                "description": "Request terminal actions in Intelligent Terminal. Use send for a simple bounded action in the current pane, open for a new empty tab or panel, and open_and_send for a new destination with input. Prefer a panel for related parallel work and a tab for independent work, a different environment, or a long-running task. Routing is automatic. Call at most once per turn, then end without assistant prose.",
                "inputSchema": super::schema::mcp_input_schema()
            }]
        }),
        "tools/call" => {
            let name = request.pointer("/params/name").and_then(Value::as_str);
            if name != Some(TOOL_NAME) {
                return Some(error_response(id, -32602, "unknown tool"));
            }
            let arguments = request
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match submit_proposal(pipe_name, capability, arguments).await {
                Ok(response) => {
                    let status = response.status;
                    let status_text = match status {
                        ProposalValidationStatus::Accepted => "accepted",
                        ProposalValidationStatus::AlreadyConsumed => "duplicate",
                        ProposalValidationStatus::Stale
                        | ProposalValidationStatus::UnknownChannel
                        | ProposalValidationStatus::HelperMismatch
                        | ProposalValidationStatus::Superseded => "stale",
                        ProposalValidationStatus::InvalidSchema
                        | ProposalValidationStatus::Rejected => "rejected",
                        ProposalValidationStatus::Unavailable => "unavailable",
                    };
                    let structured = json!({
                        "status": status_text,
                        "reason": response.reason,
                        "retryable": response.retryable
                    });
                    let text = if status == ProposalValidationStatus::Accepted {
                        "Terminal actions accepted. End the turn without additional text."
                            .to_string()
                    } else {
                        format!(
                            "Terminal action request {status_text}: {}",
                            structured["reason"]
                                .as_str()
                                .unwrap_or("no reason provided")
                        )
                    };
                    json!({
                        "content": [{ "type": "text", "text": text }],
                        "structuredContent": structured,
                        "isError": status != ProposalValidationStatus::Accepted
                    })
                }
                Err(error) => json!({
                    "content": [{
                        "type": "text",
                        "text": format!("Terminal action request unavailable: {error:#}")
                    }],
                    "structuredContent": {
                        "status": "unavailable",
                        "reason": format!("{error:#}"),
                        "retryable": false
                    },
                    "isError": true
                }),
            }
        }
        _ => return Some(error_response(id, -32601, "method not found")),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

async fn submit_proposal(
    pipe_name: &str,
    capability: &str,
    arguments: Value,
) -> Result<ProposalValidationResponse> {
    let payload = serde_json::to_string(&arguments).context("encode MCP proposal arguments")?;
    if payload.len() > super::schema::MAX_PAYLOAD_BYTES {
        anyhow::bail!(
            "proposal exceeds the {}-byte limit",
            super::schema::MAX_PAYLOAD_BYTES
        );
    }
    let pipe = open_pipe(pipe_name).await?;
    let (read_half, mut write_half) = tokio::io::split(pipe);
    let request = ProposalMcpPipeRequest {
        version: PROTOCOL_VERSION,
        capability: capability.to_string(),
        payload,
    };
    let mut request_line = serde_json::to_vec(&request)?;
    request_line.push(b'\n');
    write_half
        .write_all(&request_line)
        .await
        .context("write MCP proposal request")?;
    write_half
        .flush()
        .await
        .context("flush MCP proposal request")?;

    let mut reader = BufReader::new(read_half);
    let mut response = Vec::new();
    tokio::time::timeout(
        HELPER_RESPONSE_TIMEOUT,
        reader.read_until(b'\n', &mut response),
    )
    .await
    .context("timed out waiting for owning Helper")?
    .context("read MCP proposal response")?;
    if response.is_empty() {
        anyhow::bail!("owning Helper disconnected before responding");
    }
    serde_json::from_slice(&response).context("decode MCP proposal response")
}

async fn open_pipe(pipe_name: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    const BACKOFF_MS: &[u64] = &[20, 50, 100, 200, 500, 1000];

    for wait_ms in BACKOFF_MS {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name) {
            Ok(pipe) => return Ok(pipe),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY)
                ) =>
            {
                tokio::time::sleep(std::time::Duration::from_millis(*wait_ms)).await;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open owning Helper pipe '{pipe_name}'"));
            }
        }
    }
    anyhow::bail!("owning Helper proposal pipe is unavailable")
}

async fn write_json<W>(writer: &mut W, value: Value) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_vec(&value)?;
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await?;
    Ok(())
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stdio_lists_only_the_proposal_tool() {
        let input = br#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}
"#;
        let mut output = Vec::new();
        run(&input[..], &mut output, "unused", "unused")
            .await
            .unwrap();
        let response: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(
            response
                .pointer("/result/tools/0/name")
                .and_then(Value::as_str),
            Some(TOOL_NAME)
        );
        assert_eq!(
            response
                .pointer("/result/tools")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[tokio::test]
    async fn initialize_negotiates_a_supported_protocol_version() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": { "protocolVersion": "unsupported" }
        });
        let response = dispatch(request, "unused", "unused").await.unwrap();

        assert_eq!(
            response
                .pointer("/result/protocolVersion")
                .and_then(Value::as_str),
            Some(MCP_PROTOCOL_VERSION)
        );
    }

    #[tokio::test]
    async fn message_reader_rejects_oversized_and_unterminated_requests() {
        let oversized = vec![b'x'; MAX_MCP_MESSAGE_BYTES + 1];
        let error = read_message(&mut &oversized[..]).await.unwrap_err();
        assert!(error.to_string().contains("exceeds"));

        let error = read_message(&mut &b"{}"[..]).await.unwrap_err();
        assert!(error.to_string().contains("newline terminated"));
    }
}
