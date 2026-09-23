use super::{success, text, Invocation, Principal, Request, Response, Runtime};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(super) struct Bridge {
    pub url: String,
    pub token: String,
    shutdown: CancellationToken,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl Bridge {
    pub async fn start(runtime: Runtime, invocation: Arc<Invocation>) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let bridge = Self {
            url: format!("http://{}/mcp", listener.local_addr()?),
            token: token.clone(),
            shutdown: CancellationToken::new(),
        };
        let shutdown = bridge.shutdown.clone();
        let slots = Arc::new(Semaphore::new(16));
        tokio::spawn(async move {
            loop {
                let (socket, _) = tokio::select! {
                    _ = shutdown.cancelled() => break,
                    result = listener.accept() => match result {
                        Ok(connection) => connection,
                        Err(error) => {
                            tracing::warn!(target:"agent_center", %error, "work MCP listener failed");
                            break;
                        }
                    },
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    drop(socket);
                    continue;
                };
                let (runtime, invocation, token, shutdown) = (
                    runtime.clone(),
                    invocation.clone(),
                    token.clone(),
                    shutdown.clone(),
                );
                tokio::spawn(async move {
                    let _permit = permit;
                    tokio::select! {
                        _ = shutdown.cancelled() => {}
                        result = serve(socket, &token, &runtime, &invocation) => {
                            if let Err(error) = result {
                                tracing::debug!(target:"agent_center", %error, "work MCP request rejected");
                            }
                        }
                    }
                });
            }
        });
        Ok(bridge)
    }
}

async fn read(socket: &mut TcpStream, token: &str) -> Result<Value> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if bytes.len() > 16 * 1024 {
            bail!("MCP headers exceed limit");
        }
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let byte = socket.read_u8().await?;
        bytes.push(byte);
    };
    let header = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = header.split("\r\n");
    if lines.next() != Some("POST /mcp HTTP/1.1") {
        bail!("MCP endpoint requires POST");
    }
    let mut authorized = false;
    let mut length = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (key, value) = line.split_once(':').context("invalid MCP header")?;
        if key.eq_ignore_ascii_case("authorization") {
            if authorized {
                bail!("duplicate MCP authorization");
            }
            authorized = value.trim() == format!("Bearer {token}");
        }
        if key.eq_ignore_ascii_case("origin") {
            bail!("browser origins are not supported");
        }
        if key.eq_ignore_ascii_case("transfer-encoding") {
            bail!("chunked MCP requests unsupported");
        }
        if key.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                bail!("duplicate content length");
            }
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    if !authorized {
        bail!("MCP invocation binding required");
    }
    let length = length.context("MCP content length required")?;
    if length > 1_048_576 {
        bail!("MCP body exceeds limit");
    }
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

async fn serve(
    mut socket: TcpStream,
    token: &str,
    runtime: &Runtime,
    invocation: &Invocation,
) -> Result<()> {
    let request = tokio::time::timeout(Duration::from_secs(5), read(&mut socket, token))
        .await
        .context("work MCP request read timed out")?
        .context("work MCP request read failed")?;
    let id = request.get("id").cloned();
    if id.is_none() {
        return write_response(&mut socket, "202 Accepted", &[]).await;
    }
    let result = match request["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let Some(offered) = request["params"]["protocolVersion"]
                .as_str()
                .filter(|version| !version.is_empty())
            else {
                tracing::warn!(target:"agent_center", invocation_id = invocation.input["id"].as_str(),
                    "work MCP initialize requires a nonempty protocolVersion string");
                let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,
                    "error":{"code":-32602,"message":"protocolVersion must be a nonempty string"}}))?;
                return write(&mut socket, &body).await;
            };
            // MCP negotiates a supported alternative; a newer offer is not a transport error.
            let version = crate::agent_tools::session_mcp::negotiate_protocol_version(offered);
            let offered_log: String = offered.chars().take(64).collect();
            tracing::info!(target:"agent_center", invocation_id = invocation.input["id"].as_str(),
                offered_protocol = %offered_log, selected_protocol = version,
                "work MCP protocol negotiated");
            json!({"protocolVersion":version,"capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"agent-center-work","version":"1"}})
        }
        "ping" => json!({}),
        "tools/list" => {
            let tools = invocation.input["availableToolNames"]
                .as_array()
                .context("invocation work tools missing")?;
            let definitions = tools
                .iter()
                .filter_map(Value::as_str)
                .map(|method| {
                    super::super::schemas::tool(method)
                        .with_context(|| format!("bound tool has no protocol schema: {method}"))
                })
                .collect::<Result<Vec<_>>>()?;
            json!({"tools":definitions})
        }
        "tools/call" => {
            let response = call(runtime, invocation, &request["params"]).await;
            let response = match response {
                Ok(response) => response,
                Err(error) => Response::fail("", "EXECUTION_FAILED", format!("{error:#}")),
            };
            json!({"content":[{"type":"text","text":serde_json::to_string(&response)?}],"isError":!matches!(response.status.as_str(),"ok"|"pending"|"needs_input")})
        }
        _ => {
            let body = serde_json::to_vec(
                &json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
            )?;
            return write(&mut socket, &body).await;
        }
    };
    let body = serde_json::to_vec(&json!({"jsonrpc":"2.0","id":id,"result":result}))?;
    if body.len() > 1_048_576 {
        bail!("MCP response exceeds frame limit; use artifact references");
    }
    write(&mut socket, &body).await
}

async fn write(socket: &mut TcpStream, body: &[u8]) -> Result<()> {
    write_response(socket, "200 OK", body).await
}

async fn write_response(socket: &mut TcpStream, status: &str, body: &[u8]) -> Result<()> {
    let header = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n", body.len());
    tokio::time::timeout(Duration::from_secs(5), async {
        socket.write_all(header.as_bytes()).await?;
        socket.write_all(body).await?;
        // Even bodyless notifications need an explicit send shutdown before drop.
        // On Windows, closing with unread pipelined input can otherwise reset the
        // connection and discard an already written response at the client.
        socket.shutdown().await
    })
    .await
    .context("work MCP response write timed out")?
    .context("work MCP response write failed")?;
    Ok(())
}

async fn call(runtime: &Runtime, invocation: &Invocation, params: &Value) -> Result<Response> {
    let name = text(params, "name")?;
    let available = invocation.input["availableToolNames"]
        .as_array()
        .context("missing work tool bindings")?;
    let registered = available
        .iter()
        .filter_map(Value::as_str)
        .find(|method| method.replace('.', "_") == name)
        .context("tool is not bound to this invocation")?;
    let method =
        super::super::schemas::canonical(registered).context("bound tool method is unavailable")?;
    let arguments = &params["arguments"];
    let command_id = if super::super::schemas::is_read(method) {
        None
    } else {
        let id = text(arguments, "commandId")?;
        Uuid::parse_str(id).context("tool commandId must be UUID")?;
        Some(id.to_owned())
    };
    let mut payload = arguments["params"].clone();
    if !payload.is_object() {
        bail!("work tool params must be an object");
    }
    {
        let state = invocation.state.lock().await;
        if state.released || invocation.cancel.is_cancelled() {
            bail!("invocation work binding was revoked");
        }
        if method == "task.acknowledge"
            && payload["continuationId"].as_str() != state.current_continuation_id.as_deref()
        {
            return Ok(Response::fail(
                "",
                "STALE_DISPATCH",
                "acknowledge the exact current continuation ID",
            ));
        }
        if invocation.input.get("dispatch").is_some()
            && !state.acknowledged
            && method != "task.acknowledge"
            && !super::super::schemas::is_read(method)
        {
            return Ok(Response::fail(
                "",
                "CONTRACT_UNACKNOWLEDGED",
                "acknowledge the exact dispatch before work",
            ));
        }
    }
    if let Some(dispatch) = invocation.input.get("dispatch") {
        if matches!(
            method,
            "task.acknowledge"
                | "task.report_progress"
                | "task.request_context"
                | "result.submit"
                | "gate.submit"
                | "review.submit"
        ) {
            for (key, fixed) in [
                ("dispatchId", &dispatch["id"]),
                ("taskRevision", &dispatch["taskRevision"]),
            ] {
                if payload.get(key).is_some_and(|value| value != fixed) {
                    bail!("tool dispatch binding mismatch: {key}");
                }
                payload[key] = fixed.clone();
            }
        }
    }
    let mut request = Request::new(method, payload.clone());
    request.command_id = command_id;
    if let Some(subjects) = arguments.get("ifMatch") {
        request.if_match = serde_json::from_value(subjects.clone())?;
    }
    let call_id = Uuid::new_v4().to_string();
    runtime
        .report(
            invocation,
            "ToolActivity",
            json!({"callId":call_id,"toolName":name,"phase":"Started","summary":method}),
        )
        .await?;
    let mut response = runtime
        .inner
        .handle
        .request(
            Principal::Invocation {
                invocation_id: text(&invocation.input, "id")?.into(),
            },
            request,
        )
        .await;
    if method == "artifact.capture" && response.status == "pending" {
        let request_id = response.request_id.clone();
        let operation_id = response.operation_id.clone();
        response = match wait_response(runtime, invocation, response).await {
            Ok(mut result) => {
                result.request_id = request_id;
                result
            }
            Err(error) => {
                let mut failure = Response::fail(request_id, "OUTCOME_UNKNOWN", format!("Capture did not return its terminal receipt: {error:#}; inspect the same operation before any retry"));
                failure.operation_id = operation_id;
                failure
            }
        };
    }
    if response.status == "ok" {
        let mut state = invocation.state.lock().await;
        if method == "task.acknowledge" {
            state.acknowledged = payload["disposition"] == "Accepted";
            state.waiting = false;
            // The service-issued runtime.stop owns decline settlement and its receipt.
        }
        let key = match method {
            "result.submit" => Some("resultId"),
            "gate.submit" => Some("gateResultId"),
            "review.submit" => Some("reviewId"),
            "coordination.finish" => Some("finishId"),
            _ => None,
        };
        if let Some(id) = key.and_then(|key| response.data.as_ref()?.get(key)?.as_str()) {
            if !state.terminal_record_ids.iter().any(|record| record == id) {
                state.terminal_record_ids.push(id.into());
            }
        }
    } else if method == "task.request_context"
        && response.status == "needs_input"
        && payload["blocking"] == true
    {
        invocation.state.lock().await.waiting = true;
    }
    runtime
        .report(
            invocation,
            "ToolActivity",
            json!({"callId":call_id,"toolName":name,"phase":"Ended","summary":response.status}),
        )
        .await?;
    Ok(response)
}

pub(super) async fn await_operation(
    runtime: &Runtime,
    invocation: &Invocation,
    response: Response,
) -> Result<Value> {
    success(wait_response(runtime, invocation, response).await?)
}

async fn wait_response(
    runtime: &Runtime,
    invocation: &Invocation,
    response: Response,
) -> Result<Response> {
    if response.status != "pending" {
        return Ok(response);
    }
    let id = response
        .operation_id
        .context("pending capture operation ID missing")?;
    let principal = Principal::Invocation {
        invocation_id: text(&invocation.input, "id")?.into(),
    };
    let response = tokio::time::timeout(
        super::deadline(&invocation.input)?.min(Duration::from_secs(120)),
        runtime.inner.handle.wait_operation(principal, &id),
    )
    .await
    .with_context(|| {
        format!("capture operation {id} remains recorded; reconcile it rather than capturing again")
    })?;
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::{mpsc, Mutex};

    async fn exchange(
        runtime: &Runtime,
        invocation: Arc<Invocation>,
        request: Value,
    ) -> Result<(u16, Value)> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let address = listener.local_addr()?;
        let runtime = runtime.clone();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await?;
            serve(socket, "test-binding", &runtime, &invocation).await
        });
        let mut socket = TcpStream::connect(address).await?;
        let body = serde_json::to_vec(&request)?;
        let header = format!("POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer test-binding\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len());
        socket.write_all(header.as_bytes()).await?;
        socket.write_all(&body).await?;
        let mut response = Vec::new();
        let received =
            tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut response)).await;
        // Retain server-side evidence instead of detaching a failed request when
        // the client reports only a connection reset.
        server
            .await
            .context("MCP server task failed")?
            .with_context(|| format!("MCP server failed for method {}", request["method"]))?;
        received
            .context("MCP response read timed out")?
            .with_context(|| {
                format!("MCP response read failed for method {}", request["method"])
            })?;
        parse_response(&response)
    }

    fn parse_response(response: &[u8]) -> Result<(u16, Value)> {
        let boundary = response
            .windows(4)
            .position(|bytes| bytes == b"\r\n\r\n")
            .context("missing HTTP response")?
            + 4;
        let header = std::str::from_utf8(&response[..boundary])?;
        let status = header
            .split_whitespace()
            .nth(1)
            .context("missing HTTP status")?
            .parse()?;
        let body = if boundary == response.len() {
            Value::Null
        } else {
            serde_json::from_slice(&response[boundary..])?
        };
        Ok((status, body))
    }

    #[tokio::test]
    async fn http_responses_close_cleanly_with_queued_input() -> Result<()> {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-mcp-close-{}", Uuid::new_v4()));
        let handle = super::super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let (commands, _receiver) = mpsc::channel(1);
        let invocation = Arc::new(Invocation {
            input: json!({"id":Uuid::new_v4().to_string(),"availableToolNames":["work.get"]}),
            state: Mutex::new(Default::default()),
            report_lock: Mutex::new(()),
            cancel: CancellationToken::new(),
            commands,
        });
        let result = async {
            for (request, expected_status) in [
                (json!({"jsonrpc":"2.0","method":"notifications/initialized"}), 202),
                (json!({"jsonrpc":"2.0","id":1,"method":"ping"}), 200),
                (json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{}}), 200),
                (json!({"jsonrpc":"2.0","id":3,"method":"unknown"}), 200),
            ] {
                let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
                let address = listener.local_addr()?;
                let mut client = TcpStream::connect(address).await?;
                let body = serde_json::to_vec(&request)?;
                let token = "test-binding";
                let mut wire = format!("POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
                wire.extend_from_slice(&body);
                // Queue a second request before allowing the server to respond.
                // It must not dispatch it, but closing over these unread bytes
                // must not turn the first (especially bodyless 202) response into RST.
                let next = json!({"jsonrpc":"2.0","id":4,"method":"ping","params":{"padding":"x".repeat(4096)}});
                let next = serde_json::to_vec(&next)?;
                wire.extend_from_slice(format!("POST /mcp HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\n\r\n", next.len()).as_bytes());
                wire.extend_from_slice(&next);
                client.write_all(&wire).await?;
                let (socket, _) = listener.accept().await?;
                let (runtime, invocation) = (runtime.clone(), invocation.clone());
                let server = tokio::spawn(async move {
                    serve(socket, token, &runtime, &invocation).await
                });
                let mut response = Vec::new();
                let received = tokio::time::timeout(
                    Duration::from_secs(10),
                    client.read_to_end(&mut response),
                ).await;
                server.await??;
                received.context("queued-input response timed out")?
                    .with_context(|| format!("queued-input response reset for method {}", request["method"]))?;
                let (status, body) = parse_response(&response)?;
                assert_eq!(status, expected_status);
                assert_eq!(body.get("id"), request.get("id"));
                if expected_status == 202 {
                    assert_eq!(body, Value::Null);
                }
            }
            Ok::<_, anyhow::Error>(())
        }.await;
        runtime.shutdown().await?;
        handle.shutdown().await?;
        std::fs::remove_dir_all(&root)?;
        result
    }

    #[tokio::test]
    async fn memory_http_tools_round_trip_through_the_bound_authority() -> Result<()> {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-memory-mcp-{}", Uuid::new_v4()));
        let handle = super::super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let result = async {
            let runtime_id = runtime.inner.runtime_id.clone();
            super::super::success(runtime.request(
                Principal::Runtime { runtime_id:runtime_id.clone() },
                "runtime.register",
                json!({"runtimeInstanceId":runtime_id,"protocolVersions":[1],"capabilities":[
                    {"id":"memory-test","kinds":["Coordinate","ProduceResult","ReviewResult"],
                        "supportsContinuation":true,"supportsScopedStop":true},
                    {"id":"native-check","kinds":["EvaluateGate"],
                        "supportsContinuation":false,"supportsScopedStop":true}
                ]}),
            ).await)?;
            super::super::success(runtime.request(Principal::Human, "console.configure", json!({
                "capabilityId":"memory-test","workerCapabilityId":"memory-test",
                "checkCapabilityId":"native-check","approvedModelDestination":"Local scripted test",
                "limits":{"concurrency":1,"executionAttempts":2,"evaluationAttempts":2,
                    "coordinationTurns":4,"contextRounds":1,"executionSeconds":60,"coordinationSeconds":60}
            })).await)?;
            let submitted = super::super::success(runtime.request(Principal::Human, "conversation.submit", json!({
                "conversationId":Uuid::new_v4().to_string(),
                "clientMessageId":Uuid::new_v4().to_string(),
                "text":"Prefer concise responses across my work.","attachments":[],
                "context":{"scope":"Global","consoleSessionId":Uuid::new_v4().to_string(),"contextVersion":1}
            })).await)?;
            let effect = handle.effects().await?.into_iter()
                .find(|effect| effect.method == "runtime.invoke")
                .context("missing memory test invocation")?;
            let (commands, _receiver) = mpsc::channel(1);
            let invocation = Arc::new(Invocation {
                input:effect.params["invocation"].clone(),
                state:Mutex::new(Default::default()),report_lock:Mutex::new(()),
                cancel:CancellationToken::new(),commands,
            });
            handle.complete_effect(&effect.id, Response::ok("", json!({
                "invocationId":invocation.input["id"],"disposition":"Recorded"
            }))).await?;
            runtime.report(&invocation, "Started", json!({
                "adapterKind":"ACP","executionIdentity":"memory-test","providerSessionId":"memory-test-session"
            })).await?;
            let (_, listed) = exchange(&runtime, invocation.clone(), json!({
                "jsonrpc":"2.0","id":1,"method":"tools/list"
            })).await?;
            for name in ["memory_list","memory_store","memory_forget"] {
                assert!(listed["result"]["tools"].as_array().unwrap().iter().any(|tool| tool["name"] == name));
            }
            let source = submitted["messageId"].clone();
            let (_, stored) = exchange(&runtime, invocation.clone(), json!({
                "jsonrpc":"2.0","id":2,"method":"tools/call","params":{
                    "name":"memory_store","arguments":{"commandId":Uuid::new_v4().to_string(),"ifMatch":[],
                        "params":{"key":"response.style","scope":"User","content":"Prefer concise responses.","sourceMessageId":source}}
                }
            })).await?;
            assert_eq!(stored["result"]["isError"], false, "{stored}");
            let response: Value = serde_json::from_str(stored["result"]["content"][0]["text"].as_str().unwrap())?;
            let record = &response["data"];
            let (_, listed) = exchange(&runtime, invocation.clone(), json!({
                "jsonrpc":"2.0","id":3,"method":"tools/call",
                "params":{"name":"memory_list","arguments":{"params":{}}}
            })).await?;
            assert_eq!(listed["result"]["isError"], false, "{listed}");
            let response: Value = serde_json::from_str(listed["result"]["content"][0]["text"].as_str().unwrap())?;
            assert_eq!(response["data"]["items"], json!([record]));
            let (_, forgotten) = exchange(&runtime, invocation.clone(), json!({
                "jsonrpc":"2.0","id":4,"method":"tools/call","params":{
                    "name":"memory_forget","arguments":{"commandId":Uuid::new_v4().to_string(),
                        "ifMatch":[{"kind":"Preference","id":record["id"],"version":record["version"]}],
                        "params":{"preferenceId":record["id"],"sourceMessageId":source}}
                }
            })).await?;
            assert_eq!(forgotten["result"]["isError"], false, "{forgotten}");
            let response: Value = serde_json::from_str(forgotten["result"]["content"][0]["text"].as_str().unwrap())?;
            assert_eq!(response["data"]["status"], "Forgotten");
            assert!(response["data"].get("content").is_none());
            invocation.cancel.cancel();
            let (_, rejected) = exchange(&runtime, invocation, json!({
                "jsonrpc":"2.0","id":5,"method":"tools/call",
                "params":{"name":"memory_list","arguments":{"params":{}}}
            })).await?;
            assert_eq!(rejected["result"]["isError"], true);
            Ok::<_, anyhow::Error>(())
        }.await;
        runtime.shutdown().await?;
        handle.shutdown().await?;
        std::fs::remove_dir_all(&root)?;
        result
    }

    #[tokio::test]
    async fn http_initialize_negotiates_newer_offers_and_keeps_bound_tools_usable() -> Result<()> {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-mcp-negotiation-{}", Uuid::new_v4()));
        let handle = super::super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let (commands, _receiver) = mpsc::channel(1);
        let invocation = Arc::new(Invocation {
            input: json!({"id":Uuid::new_v4().to_string(),"availableToolNames":["work.get"]}),
            state: Mutex::new(Default::default()),
            report_lock: Mutex::new(()),
            cancel: CancellationToken::new(),
            commands,
        });
        let result = async {
            for (offered, selected) in [
                ("2024-11-05", "2024-11-05"),
                ("2025-03-26", "2025-03-26"),
                ("2025-06-18", "2025-06-18"),
                ("2025-11-25", "2025-06-18"),
                ("2099-01-01", "2025-06-18"),
            ] {
                let (status, response) = exchange(&runtime, invocation.clone(), json!({
                    "jsonrpc":"2.0","id":offered,"method":"initialize",
                    "params":{"protocolVersion":offered,"capabilities":{},"clientInfo":{"name":"regression","version":"1"}}
                })).await?;
                assert_eq!(status, 200);
                assert_eq!(response["id"], offered);
                assert_eq!(response["result"]["protocolVersion"], selected);
                assert!(response.get("error").is_none());
                let (status, _) = exchange(&runtime, invocation.clone(), json!({"jsonrpc":"2.0","method":"notifications/initialized"})).await?;
                assert_eq!(status, 202);
                let (_, tools) = exchange(&runtime, invocation.clone(), json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})).await?;
                assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 1);
                assert_eq!(tools["result"]["tools"][0]["name"], "work_get");
                let (_, ping) = exchange(&runtime, invocation.clone(), json!({"jsonrpc":"2.0","id":3,"method":"ping"})).await?;
                assert_eq!(ping["result"], json!({}));
            }
            for params in [json!({}), json!({"protocolVersion":null}), json!({"protocolVersion":1}), json!({"protocolVersion":""})] {
                let (status, response) = exchange(&runtime, invocation.clone(), json!({"jsonrpc":"2.0","id":"invalid","method":"initialize","params":params})).await?;
                assert_eq!(status, 200);
                assert_eq!(response["id"], "invalid");
                assert_eq!(response["error"]["code"], -32602);
                assert!(response.get("result").is_none());
            }
            let (_, unknown) = exchange(&runtime, invocation, json!({"jsonrpc":"2.0","id":4,"method":"unknown"})).await?;
            assert_eq!(unknown["error"]["code"], -32601);
            Ok::<_, anyhow::Error>(())
        }.await;
        runtime.shutdown().await?;
        handle.shutdown().await?;
        std::fs::remove_dir_all(&root)?;
        result
    }
}
