//! Private SDK process shared by successive, isolated Omni requests over host-owned pipes.

use super::{PlanOutput, MAX_OUTPUT, MAX_REQUEST};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::process::{Child, ChildStdin, ChildStdout};

async fn read_line(reader: &mut (impl AsyncBufRead + Unpin), limit: usize) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_until(b'\n', &mut line)
        .await?;
    ensure!(line.len() <= limit, "Omni frame exceeds limit");
    Ok(line)
}

async fn read_frame(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Value> {
    let header = read_line(reader, 128).await?;
    ensure!(!header.is_empty(), "Omni runtime disconnected");
    let length: usize = std::str::from_utf8(&header)?
        .trim()
        .strip_prefix("Content-Length:")
        .context("invalid Omni runtime frame")?
        .trim()
        .parse()?;
    ensure!(
        length > 0 && length <= MAX_OUTPUT as usize,
        "invalid Omni frame length"
    );
    ensure!(
        read_line(reader, 2).await? == b"\r\n",
        "invalid Omni frame separator"
    );
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

async fn write_frame(writer: &mut (impl AsyncWrite + Unpin), value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    writer
        .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

struct Turn {
    session_id: String,
    output: PlanOutput,
    idle: bool,
}

impl Turn {
    fn observe(&mut self, message: &Value) -> Result<()> {
        if message["method"] == "session.event"
            && message["params"]["sessionId"].as_str() == Some(&self.session_id)
        {
            let event = &message["params"]["event"];
            self.output.observe(event)?;
            self.idle |= event["type"] == "session.idle";
        }
        Ok(())
    }
}

struct Runtime {
    _child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    next_id: u64,
    session: Option<String>,
}

impl Runtime {
    async fn start() -> Result<Self> {
        let cwd = super::planning_directory().await?;
        let mut command = crate::cli::native_agent::omni_command()?;
        let mut child = command
            .args([
                "--headless",
                "--stdio",
                "--no-auto-update",
                "--disable-builtin-mcps",
                "--no-custom-instructions",
                "--log-level",
                "error",
            ])
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("start resident Omni coordinator")?;
        let input = child.stdin.take().context("missing Omni runtime input")?;
        let output = BufReader::new(child.stdout.take().context("missing Omni runtime output")?);
        Ok(Self {
            _child: child,
            input,
            output,
            next_id: 0,
            session: None,
        })
    }

    async fn receive(&mut self, turn: Option<&mut Turn>) -> Result<Value> {
        let message = read_frame(&mut self.output).await?;
        if message.get("method").is_some() && message.get("id").is_some() {
            write_frame(
                &mut self.input,
                &json!({
                    "jsonrpc":"2.0", "id":message["id"],
                    "error":{"code":-32601,"message":"Omni exposes no host capabilities"}
                }),
            )
            .await?;
            bail!("Omni runtime requested a host capability");
        }
        if let Some(turn) = turn {
            turn.observe(&message)?;
        }
        Ok(message)
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        mut turn: Option<&mut Turn>,
    ) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        write_frame(
            &mut self.input,
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
        )
        .await?;
        loop {
            let message = self.receive(turn.as_deref_mut()).await?;
            if message.get("id").is_some() {
                ensure!(
                    message["id"].as_u64() == Some(id),
                    "unexpected Omni response"
                );
                ensure!(
                    message.get("error").is_none(),
                    "Omni runtime rejected {method}"
                );
                return message
                    .get("result")
                    .cloned()
                    .context("missing Omni response result");
            }
        }
    }

    async fn prepare(&mut self) -> Result<()> {
        if self.session.is_some() {
            return Ok(());
        }
        let cwd = super::planning_directory().await?;
        let created = self
            .request(
                "session.create",
                json!({
                    "clientName":"intelligent-terminal-omni",
                    "workingDirectory":cwd,
                    "availableTools":["__intelligent_terminal_omni_no_tools__"],
                    "tools":[], "mcpServers":{},
                    "enableConfigDiscovery":false, "enableFileHooks":false,
                    "enableHostGitOperations":false, "enableSkills":false,
                    "enableSessionStore":false, "enableManagedSettings":true,
                    "memory":{"enabled":false}, "requestPermission":true
                }),
                None,
            )
            .await?;
        let id = created["sessionId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .context("missing Omni session identity")?
            .to_owned();
        self.session = Some(id.clone());
        self.request(
            "session.tools.initializeAndValidate",
            json!({"sessionId":id}),
            None,
        )
        .await?;
        self.verify_inventory(&id).await
    }

    async fn verify_inventory(&mut self, id: &str) -> Result<()> {
        let inventory = self
            .request(
                "session.tools.getCurrentMetadata",
                json!({"sessionId":id}),
                None,
            )
            .await?;
        ensure!(
            inventory["tools"].as_array().is_some_and(Vec::is_empty),
            "Omni tool isolation unavailable"
        );
        Ok(())
    }

    async fn plan(&mut self, request: &str) -> Result<Value> {
        let (context, prompt) = super::planning_prompt(request)?;
        let id = self.session.clone().context("Omni session not prepared")?;
        let mut turn = Turn {
            session_id: id.clone(),
            output: PlanOutput::default(),
            idle: false,
        };
        self.request(
            "session.send",
            json!({"sessionId":id,"prompt":prompt}),
            Some(&mut turn),
        )
        .await?;
        while !turn.idle {
            self.receive(Some(&mut turn)).await?;
        }
        self.verify_inventory(&id).await?;
        Ok(serde_json::to_value(turn.output.finish(&context)?)?)
    }

    async fn release_session(&mut self) -> Result<()> {
        if let Some(id) = self.session.take() {
            self.request("session.delete", json!({"sessionId":id}), None)
                .await?;
        }
        Ok(())
    }
}

pub(in crate::cli) async fn serve() -> Result<()> {
    let mut input = BufReader::new(tokio::io::stdin());
    let mut output = tokio::io::stdout();
    let mut runtime: Option<Runtime> = None;
    loop {
        let line = read_line(&mut input, MAX_REQUEST + 1).await?;
        if line.is_empty() {
            return Ok(());
        }
        let request = std::str::from_utf8(&line)?.trim_end();
        let warmup = request == "null";
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(90), async {
            crate::cli::native_agent::check_omni_policy()?;
            if !warmup {
                super::planning_prompt(request)?;
            }
            if runtime.is_none() {
                runtime = Some(Runtime::start().await?);
            }
            let coordinator = runtime.as_mut().context("Omni runtime unavailable")?;
            coordinator.prepare().await?;
            if warmup {
                Ok(json!({"ready":true}))
            } else {
                coordinator.plan(request).await
            }
        })
        .await
        .context("Omni planning timed out")
        .and_then(|result| result);
        let response = match result {
            Ok(plan) => plan,
            Err(error) => {
                tracing::warn!(target: "omni", %error, "resident planning failed");
                runtime = None;
                json!({"error":"Omni planning failed"})
            }
        };
        tracing::info!(target: "omni", warmup, elapsed_ms = started.elapsed().as_millis() as u64,
            succeeded = response.get("error").is_none(), "resident request completed");
        output
            .write_all(serde_json::to_string(&response)?.as_bytes())
            .await?;
        output.write_all(b"\n").await?;
        output.flush().await?;
        if response.get("error").is_some() {
            // Exiting also closes the broker's descendant job. Do not retain
            // MCP/runtime descendants from a failed connection across retries.
            return Ok(());
        }
        // The result is already available to Terminal. Never keep the conversation
        // for the next operation, even when it belongs to the same tab.
        if !warmup {
            if let Some(coordinator) = runtime.as_mut() {
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(10), coordinator.release_session())
                        .await,
                    Ok(Ok(()))
                ) {
                    tracing::warn!(target: "omni", "resetting runtime after session cleanup failed");
                    bail!("Omni session cleanup failed");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_truncated_and_invalid_frames() {
        for data in [
            b"Content-Length: 2097153\r\n\r\n".as_slice(),
            b"Content-Length: 9\r\n\r\n{}".as_slice(),
            b"Content-Length: 2\r\n\n{}".as_slice(),
            b"not a frame\r\n\r\n{}".as_slice(),
        ] {
            assert!(read_frame(&mut BufReader::new(data)).await.is_err());
        }
        assert!(read_line(&mut BufReader::new(b"12345\n".as_slice()), 4)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn reads_successive_frames_without_consuming_the_next_reply() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &json!({"id":1,"result":{}}))
            .await
            .unwrap();
        write_frame(&mut bytes, &json!({"id":2,"result":{"tools":[]}}))
            .await
            .unwrap();
        let mut input = BufReader::new(bytes.as_slice());
        assert_eq!(read_frame(&mut input).await.unwrap()["id"], 1);
        assert_eq!(read_frame(&mut input).await.unwrap()["id"], 2);
    }

    #[test]
    fn rejects_tools_and_errors_and_does_not_mix_sessions() {
        let mut turn = Turn {
            session_id: "one".into(),
            output: PlanOutput::default(),
            idle: false,
        };
        let event = |id: &str, kind: &str| {
            json!({
                "method":"session.event","params":{"sessionId":id,"event":{"type":kind}}
            })
        };
        turn.observe(&event("other", "session.idle")).unwrap();
        assert!(!turn.idle);
        assert!(turn.observe(&event("one", "tool.execution_start")).is_err());
        assert!(turn.observe(&event("one", "session.error")).is_err());
        turn.observe(&event("one", "session.idle")).unwrap();
        assert!(turn.idle);
        assert!(turn.output.finish(&json!({})).is_err());
    }
}
