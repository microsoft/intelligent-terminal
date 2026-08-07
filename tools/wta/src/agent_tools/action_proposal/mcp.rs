use std::future::Future;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::channel::ProposalValidationStatus;
use super::pipe::ProposalValidationResponse;

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_MCP_PROTOCOL_VERSIONS: &[&str] =
    &["2024-11-05", "2025-03-26", MCP_PROTOCOL_VERSION];
const TOOL_NAME: &str = "request_terminal_actions";
pub const SERVER_NAME_PREFIX: &str = "intellterm_";
pub const SERVER_ID_HEX_LEN: usize = 20;
pub const HELPER_REQUEST_METHOD: &str = "_intellterm.wta/request_terminal_actions";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperRequest {
    pub session_id: String,
    pub arguments: Value,
}

pub fn helper_method_matches(method: &str) -> bool {
    method.trim_start_matches('_') == HELPER_REQUEST_METHOD.trim_start_matches('_')
}

pub fn server_name_matches(name: &str) -> bool {
    if name == "intelligent_terminal" {
        return true;
    }
    name.strip_prefix(SERVER_NAME_PREFIX)
        .is_some_and(|server_id| {
            server_id.len() == SERVER_ID_HEX_LEN
                && server_id
                    .chars()
                    .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
        })
}

pub async fn dispatch<F, Fut>(request: Value, submit: F) -> Option<Value>
where
    F: FnOnce(Value) -> Fut,
    Fut: Future<Output = anyhow::Result<ProposalValidationResponse>>,
{
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
                "description": "Request one terminal action in Intelligent Terminal. Use send for a simple bounded action in the current pane, open for a new empty tab or panel, and open_and_send for a new destination with input. Prefer a panel for related parallel work and a tab for independent work, a different environment, or a long-running task. Routing is automatic. Call at most once per turn, then end without assistant prose.",
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
            match submit(arguments).await {
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

pub fn error_response(id: Value, code: i64, message: &str) -> Value {
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
    async fn lists_only_the_proposal_tool() {
        let response = dispatch(
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}),
            |_| async { unreachable!() },
        )
        .await
        .unwrap();
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
        let response = dispatch(request, |_| async { unreachable!() })
            .await
            .unwrap();

        assert_eq!(
            response
                .pointer("/result/protocolVersion")
                .and_then(Value::as_str),
            Some(MCP_PROTOCOL_VERSION)
        );
    }
}
