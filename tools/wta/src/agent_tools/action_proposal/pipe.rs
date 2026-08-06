use super::channel::{
    ProposalChannel, ProposalChannelManager, ProposalFinalStatus, ProposalValidationStatus,
    ValidationContext,
};
use super::schema::MAX_PAYLOAD_BYTES;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::NamedPipeServer;
use tokio::sync::{mpsc, oneshot};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = MAX_PAYLOAD_BYTES * 6 + 1024;
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(10);
const USER_DECISION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalPipeRequest {
    pub version: u32,
    pub channel: String,
    pub payload: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposalMcpPipeRequest {
    pub version: u32,
    pub capability: String,
    pub payload: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum IncomingProposalPipeRequest {
    Cli(ProposalPipeRequest),
    Mcp(ProposalMcpPipeRequest),
}

enum ProposalRequestRoute {
    Cli(ProposalChannel),
    Mcp(String),
}

enum ProposalCompletion {
    Detached,
    AwaitFinal(oneshot::Receiver<ProposalFinalStatus>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalPayloadSource {
    Cli,
    Mcp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProposalValidationResponse {
    pub phase: ValidationPhase,
    pub status: ProposalValidationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposal_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub retryable: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ValidationPhase {
    Validation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProposalFinalResponse {
    pub phase: FinalPhase,
    pub status: ProposalFinalStatus,
    pub proposal_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinalPhase {
    Final,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalValidationDecision {
    pub status: ProposalValidationStatus,
    pub reason: Option<String>,
    pub retryable: bool,
}

impl ProposalValidationDecision {
    pub fn accepted() -> Self {
        Self {
            status: ProposalValidationStatus::Accepted,
            reason: None,
            retryable: false,
        }
    }
}

pub enum ProposalPipeEvent {
    Validate {
        context: ValidationContext,
        payload: String,
        source: ProposalPayloadSource,
        responder: oneshot::Sender<ProposalValidationDecision>,
    },
    Commit {
        proposal_id: String,
        responder: oneshot::Sender<bool>,
    },
    Invalidate {
        proposal_id: String,
        session_id: String,
    },
}

pub async fn run_server(
    manager: Arc<ProposalChannelManager>,
    event_tx: mpsc::UnboundedSender<ProposalPipeEvent>,
) -> Result<()> {
    let pipe_name = manager.pipe_name();
    let security =
        super::pipe_security::build_required().context("build hardened proposal pipe security")?;
    let mut server = super::pipe_security::create_server(&pipe_name, true, Some(&security))
        .with_context(|| format!("create proposal pipe '{pipe_name}'"))?;
    tracing::info!(
        target: "proposal_pipe",
        pipe = %pipe_name,
        "proposal pipe listening"
    );

    loop {
        server
            .connect()
            .await
            .with_context(|| format!("connect proposal pipe '{pipe_name}'"))?;
        let connected = std::mem::replace(
            &mut server,
            super::pipe_security::create_server(&pipe_name, false, Some(&security))
                .with_context(|| format!("create follow-up proposal pipe '{pipe_name}'"))?,
        );
        let manager = Arc::clone(&manager);
        let event_tx = event_tx.clone();
        tokio::task::spawn_local(async move {
            if let Err(error) = serve_connection(connected, manager, event_tx).await {
                tracing::warn!(
                    target: "proposal_pipe",
                    error = %format!("{error:#}"),
                    "proposal pipe connection failed"
                );
            }
        });
    }
}

async fn serve_connection(
    pipe: NamedPipeServer,
    manager: Arc<ProposalChannelManager>,
    event_tx: mpsc::UnboundedSender<ProposalPipeEvent>,
) -> Result<()> {
    let (read_half, mut write_half) = tokio::io::split(pipe);
    let frame = read_frame(read_half).await?;
    let request: IncomingProposalPipeRequest = match serde_json::from_slice(&frame) {
        Ok(request) => request,
        Err(error) => {
            return write_validation_failure(
                &mut write_half,
                ProposalValidationStatus::Rejected,
                format!("invalid request frame: {error}"),
                false,
            )
            .await;
        }
    };
    let (version, payload, route, source) = match request {
        IncomingProposalPipeRequest::Cli(request) => {
            let channel = match request.channel.parse::<ProposalChannel>() {
                Ok(channel) => channel,
                Err(error) => {
                    return write_validation_failure(
                        &mut write_half,
                        ProposalValidationStatus::UnknownChannel,
                        error.to_string(),
                        false,
                    )
                    .await;
                }
            };
            (
                request.version,
                request.payload,
                ProposalRequestRoute::Cli(channel),
                ProposalPayloadSource::Cli,
            )
        }
        IncomingProposalPipeRequest::Mcp(request) => (
            request.version,
            request.payload,
            ProposalRequestRoute::Mcp(request.capability),
            ProposalPayloadSource::Mcp,
        ),
    };
    if version != PROTOCOL_VERSION {
        return write_validation_failure(
            &mut write_half,
            ProposalValidationStatus::Rejected,
            format!("unsupported proposal pipe version {version} (expected {PROTOCOL_VERSION})"),
            false,
        )
        .await;
    }
    if payload.len() > MAX_PAYLOAD_BYTES {
        return write_validation_failure(
            &mut write_half,
            ProposalValidationStatus::InvalidSchema,
            format!("payload exceeds the {MAX_PAYLOAD_BYTES}-byte inline limit"),
            false,
        )
        .await;
    }
    let context = match route {
        ProposalRequestRoute::Cli(channel) => manager.begin_validation(&channel),
        ProposalRequestRoute::Mcp(capability) => manager.begin_mcp_validation(&capability),
    };
    let context = match context {
        Ok(context) => context,
        Err(failure) => {
            return write_validation_failure(
                &mut write_half,
                failure.status,
                failure.reason.to_string(),
                failure.retryable,
            )
            .await;
        }
    };
    let proposal_id = context.proposal_id.clone();
    let session_id = context.binding.session_id.clone();
    let (validation_tx, validation_rx) = oneshot::channel();
    if event_tx
        .send(ProposalPipeEvent::Validate {
            context,
            payload,
            source,
            responder: validation_tx,
        })
        .is_err()
    {
        manager.reject_validation(&proposal_id, false);
        return write_validation_failure(
            &mut write_half,
            ProposalValidationStatus::Unavailable,
            "Helper UI is unavailable".to_string(),
            false,
        )
        .await;
    }
    let decision = match tokio::time::timeout(VALIDATION_TIMEOUT, validation_rx).await {
        Ok(Ok(decision)) => decision,
        Ok(Err(_)) => ProposalValidationDecision {
            status: ProposalValidationStatus::Unavailable,
            reason: Some("Helper dropped the validation response".to_string()),
            retryable: false,
        },
        Err(_) => ProposalValidationDecision {
            status: ProposalValidationStatus::Unavailable,
            reason: Some("Helper validation timed out".to_string()),
            retryable: false,
        },
    };
    if decision.status != ProposalValidationStatus::Accepted {
        let retryable = manager.reject_validation(&proposal_id, decision.retryable);
        return write_response(
            &mut write_half,
            &ProposalValidationResponse {
                phase: ValidationPhase::Validation,
                status: decision.status,
                proposal_id: None,
                reason: decision.reason,
                retryable,
            },
        )
        .await;
    }

    let completion = if source == ProposalPayloadSource::Mcp {
        if !manager.accept_validation_detached(&proposal_id) {
            return write_validation_failure(
                &mut write_half,
                ProposalValidationStatus::Stale,
                "proposal was invalidated while validation completed".to_string(),
                false,
            )
            .await;
        }
        ProposalCompletion::Detached
    } else {
        let (final_tx, final_rx) = oneshot::channel();
        if !manager.accept_validation(&proposal_id, final_tx) {
            return write_validation_failure(
                &mut write_half,
                ProposalValidationStatus::Stale,
                "proposal was invalidated while validation completed".to_string(),
                false,
            )
            .await;
        }
        ProposalCompletion::AwaitFinal(final_rx)
    };

    let (commit_tx, commit_rx) = oneshot::channel();
    if event_tx
        .send(ProposalPipeEvent::Commit {
            proposal_id: proposal_id.clone(),
            responder: commit_tx,
        })
        .is_err()
    {
        manager.resolve_final(&proposal_id, ProposalFinalStatus::Unavailable);
        return write_validation_failure(
            &mut write_half,
            ProposalValidationStatus::Unavailable,
            "Helper UI is unavailable".to_string(),
            false,
        )
        .await;
    }
    match tokio::time::timeout(VALIDATION_TIMEOUT, commit_rx).await {
        Ok(Ok(true)) => {}
        Ok(Ok(false)) => {
            manager.resolve_final(&proposal_id, ProposalFinalStatus::Cancelled);
            return write_validation_failure(
                &mut write_half,
                ProposalValidationStatus::Stale,
                "proposal became stale before the card could be presented".to_string(),
                false,
            )
            .await;
        }
        Ok(Err(_)) | Err(_) => {
            manager.resolve_final(&proposal_id, ProposalFinalStatus::Unavailable);
            return write_validation_failure(
                &mut write_half,
                ProposalValidationStatus::Unavailable,
                "Helper did not confirm that the card was presented".to_string(),
                false,
            )
            .await;
        }
    }

    if let Err(error) = write_response(
        &mut write_half,
        &ProposalValidationResponse {
            phase: ValidationPhase::Validation,
            status: ProposalValidationStatus::Accepted,
            proposal_id: Some(proposal_id.clone()),
            reason: None,
            retryable: false,
        },
    )
    .await
    {
        manager.resolve_final(&proposal_id, ProposalFinalStatus::Unavailable);
        let _ = event_tx.send(ProposalPipeEvent::Invalidate {
            proposal_id,
            session_id,
        });
        return Err(error);
    }
    let final_rx = match completion {
        ProposalCompletion::Detached => return Ok(()),
        ProposalCompletion::AwaitFinal(final_rx) => final_rx,
    };

    let final_status = match tokio::time::timeout(USER_DECISION_TIMEOUT, final_rx).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => ProposalFinalStatus::Unavailable,
        Err(_) => {
            manager.resolve_final(&proposal_id, ProposalFinalStatus::TimedOut);
            ProposalFinalStatus::TimedOut
        }
    };
    if !matches!(
        final_status,
        ProposalFinalStatus::Confirmed | ProposalFinalStatus::Cancelled
    ) {
        let _ = event_tx.send(ProposalPipeEvent::Invalidate {
            proposal_id: proposal_id.clone(),
            session_id,
        });
    }
    write_response(
        &mut write_half,
        &ProposalFinalResponse {
            phase: FinalPhase::Final,
            status: final_status,
            proposal_id,
        },
    )
    .await
}

async fn read_frame<R>(reader: R) -> Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut frame = Vec::new();
    let mut reader = BufReader::new(reader).take(MAX_FRAME_BYTES as u64 + 1);
    let bytes_read = reader
        .read_until(b'\n', &mut frame)
        .await
        .context("read proposal request frame")?;
    if bytes_read == 0 {
        anyhow::bail!("proposal client disconnected before sending a frame");
    }
    if frame.len() > MAX_FRAME_BYTES {
        anyhow::bail!("proposal request frame exceeds {MAX_FRAME_BYTES} bytes");
    }
    if frame.last() != Some(&b'\n') {
        anyhow::bail!("proposal request frame is not newline terminated");
    }
    frame.pop();
    if frame.last() == Some(&b'\r') {
        frame.pop();
    }
    Ok(frame)
}

async fn write_validation_failure<W>(
    writer: &mut W,
    status: ProposalValidationStatus,
    reason: String,
    retryable: bool,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_response(
        writer,
        &ProposalValidationResponse {
            phase: ValidationPhase::Validation,
            status,
            proposal_id: None,
            reason: Some(reason),
            retryable,
        },
    )
    .await
}

async fn write_response<W, T>(writer: &mut W, response: &T) -> Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut encoded = serde_json::to_vec(response).context("encode proposal pipe response")?;
    encoded.push(b'\n');
    writer
        .write_all(&encoded)
        .await
        .context("write proposal pipe response")?;
    writer.flush().await.context("flush proposal pipe response")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_and_two_response_phases_have_stable_json_shapes() {
        let request = ProposalPipeRequest {
            version: PROTOCOL_VERSION,
            channel: "v1.helper.turn".to_string(),
            payload: "{}".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&request).unwrap(),
            r#"{"version":1,"channel":"v1.helper.turn","payload":"{}"}"#
        );
        let mcp_request = ProposalMcpPipeRequest {
            version: PROTOCOL_VERSION,
            capability: "capability".to_string(),
            payload: "{}".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&mcp_request).unwrap(),
            r#"{"version":1,"capability":"capability","payload":"{}"}"#
        );

        let validation = ProposalValidationResponse {
            phase: ValidationPhase::Validation,
            status: ProposalValidationStatus::Accepted,
            proposal_id: Some("proposal".to_string()),
            reason: None,
            retryable: false,
        };
        assert_eq!(
            serde_json::to_string(&validation).unwrap(),
            r#"{"phase":"validation","status":"accepted","proposal_id":"proposal","retryable":false}"#
        );

        let final_response = ProposalFinalResponse {
            phase: FinalPhase::Final,
            status: ProposalFinalStatus::Confirmed,
            proposal_id: "proposal".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&final_response).unwrap(),
            r#"{"phase":"final","status":"confirmed","proposal_id":"proposal"}"#
        );

        let worst_case_request = serde_json::to_vec(&ProposalPipeRequest {
            version: PROTOCOL_VERSION,
            channel: "v1.helper.turn".to_string(),
            payload: "\\".repeat(MAX_PAYLOAD_BYTES),
        })
        .unwrap();
        assert!(worst_case_request.len() < MAX_FRAME_BYTES);
    }

    #[tokio::test]
    async fn frame_reader_requires_newline_and_enforces_limit() {
        let valid = read_frame(std::io::Cursor::new(b"{}\n".to_vec()))
            .await
            .unwrap();
        assert_eq!(valid, b"{}");

        let missing_newline = read_frame(std::io::Cursor::new(b"{}".to_vec()))
            .await
            .unwrap_err();
        assert!(missing_newline.to_string().contains("newline terminated"));

        let oversized = vec![b'x'; MAX_FRAME_BYTES + 1];
        let error = read_frame(std::io::Cursor::new(oversized))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[tokio::test]
    async fn named_pipe_round_trip_without_permission_returns_both_phases() {
        let manager = Arc::new(ProposalChannelManager::new());
        let payload = r#"{"schema_version":1,"origin":"terminal_agent","choices":[{"choice":1,"title":"run","rationale":"","actions":[{"type":"send","input":"echo ok"}]}]}"#;
        let channel = manager
            .issue("session".to_string(), 1, None, false)
            .unwrap();
        let pipe_name = manager.pipe_name();
        let security = super::super::pipe_security::build_required().unwrap();
        let server =
            super::super::pipe_security::create_server(&pipe_name, true, Some(&security)).unwrap();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let server_manager = Arc::clone(&manager);
        let server_future = async move {
            server.connect().await.unwrap();
            serve_connection(server, server_manager, event_tx)
                .await
                .unwrap();
        };
        let event_manager = Arc::clone(&manager);
        let event_future = async move {
            let proposal_id = match event_rx.recv().await.unwrap() {
                ProposalPipeEvent::Validate {
                    context, responder, ..
                } => {
                    let proposal_id = context.proposal_id;
                    responder
                        .send(ProposalValidationDecision::accepted())
                        .unwrap();
                    proposal_id
                }
                ProposalPipeEvent::Commit { .. } => panic!("commit arrived before validation"),
                ProposalPipeEvent::Invalidate { .. } => {
                    panic!("invalidation arrived before validation")
                }
            };
            match event_rx.recv().await.unwrap() {
                ProposalPipeEvent::Commit {
                    proposal_id: committed,
                    responder,
                } => {
                    assert_eq!(committed, proposal_id);
                    responder.send(true).unwrap();
                }
                ProposalPipeEvent::Validate { .. } => panic!("duplicate validation event"),
                ProposalPipeEvent::Invalidate { .. } => {
                    panic!("unexpected invalidation for confirmed proposal")
                }
            }
            assert!(event_manager.resolve_final(&proposal_id, ProposalFinalStatus::Confirmed));
        };
        let client_future = async move {
            let client = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&pipe_name)
                .unwrap();
            let (read_half, mut write_half) = tokio::io::split(client);
            let mut request = serde_json::to_vec(&ProposalPipeRequest {
                version: PROTOCOL_VERSION,
                channel: channel.to_string(),
                payload: payload.to_string(),
            })
            .unwrap();
            request.push(b'\n');
            write_half.write_all(&request).await.unwrap();
            write_half.flush().await.unwrap();

            let mut lines = BufReader::new(read_half).lines();
            let validation: ProposalValidationResponse =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(validation.status, ProposalValidationStatus::Accepted);
            let proposal_id = validation.proposal_id.unwrap();
            let final_response: ProposalFinalResponse =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(final_response.status, ProposalFinalStatus::Confirmed);
            assert_eq!(final_response.proposal_id, proposal_id);
        };

        tokio::join!(server_future, event_future, client_future);
    }

    #[tokio::test]
    async fn mcp_round_trip_returns_after_card_commit() {
        let manager = Arc::new(ProposalChannelManager::new());
        let mcp = manager.prepare_mcp_session();
        assert!(manager.bind_mcp_session(&mcp.capability, "session".to_string()));
        manager
            .issue("session".to_string(), 1, None, false)
            .unwrap();
        let pipe_name = manager.pipe_name();
        let security = super::super::pipe_security::build_required().unwrap();
        let server =
            super::super::pipe_security::create_server(&pipe_name, true, Some(&security)).unwrap();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        let server_manager = Arc::clone(&manager);
        let server_future = async move {
            server.connect().await.unwrap();
            serve_connection(server, server_manager, event_tx)
                .await
                .unwrap();
        };
        let event_manager = Arc::clone(&manager);
        let event_future = async move {
            let proposal_id = match event_rx.recv().await.unwrap() {
                ProposalPipeEvent::Validate {
                    context,
                    source,
                    responder,
                    ..
                } => {
                    assert_eq!(source, ProposalPayloadSource::Mcp);
                    let proposal_id = context.proposal_id;
                    responder
                        .send(ProposalValidationDecision::accepted())
                        .unwrap();
                    proposal_id
                }
                ProposalPipeEvent::Commit { .. } => panic!("commit arrived before validation"),
                ProposalPipeEvent::Invalidate { .. } => {
                    panic!("invalidation arrived before validation")
                }
            };
            match event_rx.recv().await.unwrap() {
                ProposalPipeEvent::Commit {
                    proposal_id: committed,
                    responder,
                } => {
                    assert_eq!(committed, proposal_id);
                    responder.send(true).unwrap();
                }
                ProposalPipeEvent::Validate { .. } => panic!("duplicate validation event"),
                ProposalPipeEvent::Invalidate { .. } => {
                    panic!("unexpected invalidation for committed proposal")
                }
            }
            let claim = event_manager.claim_confirmation(&proposal_id).unwrap();
            event_manager.finalize_confirmation(claim, ProposalFinalStatus::Confirmed);
        };
        let client_future = async move {
            let client = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&pipe_name)
                .unwrap();
            let (read_half, mut write_half) = tokio::io::split(client);
            let mut request = serde_json::to_vec(&ProposalMcpPipeRequest {
                version: PROTOCOL_VERSION,
                capability: mcp.capability,
                payload:
                    r#"{"choices":[{"title":"run","actions":[{"type":"send","input":"echo ok"}]}]}"#
                        .to_string(),
            })
            .unwrap();
            request.push(b'\n');
            write_half.write_all(&request).await.unwrap();
            write_half.flush().await.unwrap();

            let mut lines = BufReader::new(read_half).lines();
            let validation: ProposalValidationResponse =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert_eq!(validation.status, ProposalValidationStatus::Accepted);
            assert!(validation.proposal_id.is_some());
            assert!(
                lines.next_line().await.unwrap().is_none(),
                "MCP must not wait for the user's final card decision"
            );
        };

        tokio::join!(server_future, event_future, client_future);
    }
}
