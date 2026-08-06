use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol as acp;
use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::MasterStateInner;
use crate::agent_tools::action_proposal::mcp::HelperRequest;
use crate::agent_tools::action_proposal::pipe::ProposalValidationResponse;

const ENDPOINT_PATH: &str = "/mcp";
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const HELPER_TIMEOUT: Duration = Duration::from_secs(25);
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

#[derive(Clone)]
pub(super) struct PendingCapability {
    secret: String,
    hash: [u8; 32],
}

#[derive(Default)]
pub(super) struct CapabilityRegistry {
    routes: Mutex<CapabilityRoutes>,
}

#[derive(Default)]
struct CapabilityRoutes {
    by_capability: HashMap<[u8; 32], Option<acp::schema::v1::SessionId>>,
    by_session: HashMap<acp::schema::v1::SessionId, [u8; 32]>,
}

impl CapabilityRegistry {
    pub(super) async fn prepare(
        &self,
        session_id: Option<acp::schema::v1::SessionId>,
    ) -> PendingCapability {
        let secret = Uuid::new_v4().simple().to_string();
        let hash = hash_secret(&secret);
        self.routes
            .lock()
            .await
            .by_capability
            .insert(hash, session_id);
        PendingCapability { secret, hash }
    }

    pub(super) async fn bind(
        &self,
        pending: &PendingCapability,
        session_id: acp::schema::v1::SessionId,
    ) -> bool {
        let mut routes = self.routes.lock().await;
        if !routes.by_capability.contains_key(&pending.hash) {
            return false;
        }
        if let Some(old) = routes.by_session.insert(session_id.clone(), pending.hash) {
            routes.by_capability.remove(&old);
        }
        routes.by_capability.insert(pending.hash, Some(session_id));
        true
    }

    pub(super) async fn cancel(&self, pending: &PendingCapability) {
        self.routes.lock().await.by_capability.remove(&pending.hash);
    }

    async fn resolve(&self, secret: &str) -> CapabilityResolution {
        match self
            .routes
            .lock()
            .await
            .by_capability
            .get(&hash_secret(secret))
            .cloned()
        {
            Some(Some(session_id)) => CapabilityResolution::Bound(session_id),
            Some(None) => CapabilityResolution::Pending,
            None => CapabilityResolution::Unknown,
        }
    }
}

enum CapabilityResolution {
    Bound(acp::schema::v1::SessionId),
    Pending,
    Unknown,
}

pub(super) fn server_config(
    endpoint: &str,
    pending: &PendingCapability,
) -> acp::schema::v1::McpServer {
    acp::schema::v1::McpServer::Http(
        acp::schema::v1::McpServerHttp::new("intelligent_terminal", endpoint).headers(vec![
            acp::schema::v1::HttpHeader::new("Authorization", format!("Bearer {}", pending.secret)),
        ]),
    )
}

pub(super) async fn run(listener: TcpListener, state: Arc<MasterStateInner>) -> Result<()> {
    let address = listener.local_addr().context("read proposal MCP address")?;
    tracing::info!(
        target: "proposal_mcp",
        address = %address,
        "master proposal MCP HTTP endpoint listening"
    );
    let connections = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    loop {
        let (stream, peer) = listener
            .accept()
            .await
            .context("accept proposal MCP HTTP")?;
        if !peer.ip().is_loopback() {
            tracing::warn!(
                target: "proposal_mcp",
                peer = %peer,
                "rejecting non-loopback proposal MCP connection"
            );
            continue;
        }
        let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
            tracing::warn!(
                target: "proposal_mcp",
                "rejecting proposal MCP connection at concurrency limit"
            );
            continue;
        };
        let state = Arc::clone(&state);
        tokio::task::spawn_local(async move {
            let _permit = permit;
            if let Err(error) = serve_connection(stream, address, state).await {
                tracing::debug!(
                    target: "proposal_mcp",
                    error = %format!("{error:#}"),
                    "proposal MCP HTTP connection failed"
                );
            }
        });
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    address: std::net::SocketAddr,
    state: Arc<MasterStateInner>,
) -> Result<()> {
    let request = match tokio::time::timeout(HTTP_REQUEST_TIMEOUT, read_request(&mut stream)).await
    {
        Ok(Ok(request)) => request,
        Ok(Err(error)) => {
            write_response(
                &mut stream,
                400,
                "Bad Request",
                "text/plain",
                error.to_string().as_bytes(),
            )
            .await?;
            return Ok(());
        }
        Err(_) => {
            write_response(
                &mut stream,
                408,
                "Request Timeout",
                "text/plain",
                b"request timed out",
            )
            .await?;
            return Ok(());
        }
    };
    if request.path != ENDPOINT_PATH {
        write_response(&mut stream, 404, "Not Found", "text/plain", b"not found").await?;
        return Ok(());
    }
    let expected_host = address.to_string();
    let expected_localhost = format!("localhost:{}", address.port());
    if !matches!(
        request.header("host"),
        Some(host) if host == expected_host || host.eq_ignore_ascii_case(&expected_localhost)
    ) {
        write_response(&mut stream, 403, "Forbidden", "text/plain", b"invalid host").await?;
        return Ok(());
    }
    if !origin_is_allowed(request.header("origin")) {
        write_response(
            &mut stream,
            403,
            "Forbidden",
            "text/plain",
            b"invalid origin",
        )
        .await?;
        return Ok(());
    }
    let Some(secret) = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        write_response(
            &mut stream,
            401,
            "Unauthorized",
            "text/plain",
            b"missing capability",
        )
        .await?;
        return Ok(());
    };
    let capability = state.proposal_mcp_capabilities.resolve(secret).await;
    if matches!(capability, CapabilityResolution::Unknown) {
        write_response(
            &mut stream,
            401,
            "Unauthorized",
            "text/plain",
            b"unknown capability",
        )
        .await?;
        return Ok(());
    }

    match request.method.as_str() {
        "GET" | "DELETE" => {
            write_response(
                &mut stream,
                405,
                "Method Not Allowed",
                "text/plain",
                b"server-initiated streams are not supported",
            )
            .await?;
        }
        "POST" => {
            if !request.header("content-type").is_some_and(|value| {
                value.eq_ignore_ascii_case("application/json")
                    || value.to_ascii_lowercase().starts_with("application/json;")
            }) {
                write_response(
                    &mut stream,
                    415,
                    "Unsupported Media Type",
                    "text/plain",
                    b"Content-Type must be application/json",
                )
                .await?;
                return Ok(());
            }
            if let Some(version) = request.header("mcp-protocol-version") {
                if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version) {
                    write_response(
                        &mut stream,
                        400,
                        "Bad Request",
                        "text/plain",
                        b"unsupported MCP protocol version",
                    )
                    .await?;
                    return Ok(());
                }
            }
            let message: Value = match serde_json::from_slice(&request.body) {
                Ok(message) => message,
                Err(_) => {
                    let body = serde_json::to_vec(
                        &crate::agent_tools::action_proposal::mcp::error_response(
                            Value::Null,
                            -32700,
                            "parse error",
                        ),
                    )?;
                    write_response(&mut stream, 400, "Bad Request", "application/json", &body)
                        .await?;
                    return Ok(());
                }
            };
            let response =
                crate::agent_tools::action_proposal::mcp::dispatch(message, |arguments| {
                    submit_to_helper(&state, capability, arguments)
                })
                .await;
            if let Some(response) = response {
                let body = serde_json::to_vec(&response)?;
                write_response(&mut stream, 200, "OK", "application/json", &body).await?;
            } else {
                write_empty_response(&mut stream, 202, "Accepted").await?;
            }
        }
        _ => {
            write_response(
                &mut stream,
                405,
                "Method Not Allowed",
                "text/plain",
                b"method not allowed",
            )
            .await?;
        }
    }
    Ok(())
}

async fn submit_to_helper(
    state: &MasterStateInner,
    capability: CapabilityResolution,
    arguments: Value,
) -> Result<ProposalValidationResponse> {
    let session_id = match capability {
        CapabilityResolution::Bound(session_id) => session_id,
        CapabilityResolution::Pending => anyhow::bail!("ACP session is not bound yet"),
        CapabilityResolution::Unknown => anyhow::bail!("MCP capability is unknown"),
    };
    let route = {
        let routes = state.session_to_helper.lock().await;
        routes.get(&session_id).cloned()
    }
    .context("owning Helper is disconnected")?;
    let forwarder = route
        .forwarder
        .context("owning Helper route has no forwarder")?;
    let params = serde_json::value::to_raw_value(&HelperRequest {
        session_id: session_id.to_string(),
        arguments,
    })
    .context("encode Helper proposal request")?;
    let request = acp::schema::v1::ExtRequest::new(
        crate::agent_tools::action_proposal::mcp::HELPER_REQUEST_METHOD,
        params.into(),
    );
    tracing::info!(
        target: "proposal_mcp",
        helper_id = ?route.helper_id,
        session_id = %session_id,
        "routing terminal action request to owning Helper"
    );
    let response = tokio::time::timeout(HELPER_TIMEOUT, forwarder.ext_method(request))
        .await
        .context("timed out waiting for owning Helper")?
        .context("owning Helper rejected terminal action request")?;
    serde_json::from_str(response.0.get()).context("decode Helper proposal response")
}

struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

async fn read_request(stream: &mut TcpStream) -> Result<HttpRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if bytes.len() >= MAX_HEADER_BYTES {
            anyhow::bail!("HTTP headers exceed {MAX_HEADER_BYTES} bytes");
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.context("read HTTP request")?;
        if read == 0 {
            anyhow::bail!("HTTP client disconnected before headers");
        }
        bytes.extend_from_slice(&chunk[..read]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            if index + 4 > MAX_HEADER_BYTES {
                anyhow::bail!("HTTP headers exceed {MAX_HEADER_BYTES} bytes");
            }
            break index + 4;
        }
        if bytes.len() >= MAX_HEADER_BYTES {
            anyhow::bail!("HTTP headers exceed {MAX_HEADER_BYTES} bytes");
        }
    };
    let head = std::str::from_utf8(&bytes[..header_end]).context("HTTP headers are not UTF-8")?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line
        .next()
        .context("missing HTTP method")?
        .to_string();
    let path = request_line
        .next()
        .context("missing HTTP path")?
        .to_string();
    let version = request_line.next().context("missing HTTP version")?;
    if request_line.next().is_some() {
        anyhow::bail!("malformed HTTP request line");
    }
    if version != "HTTP/1.1" {
        anyhow::bail!("only HTTP/1.1 is supported");
    }
    let mut headers = HashMap::new();
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').context("malformed HTTP header")?;
        if headers
            .insert(name.trim().to_ascii_lowercase(), value.trim().to_string())
            .is_some()
        {
            anyhow::bail!("duplicate HTTP header");
        }
    }
    if headers.contains_key("transfer-encoding") {
        anyhow::bail!("Transfer-Encoding is not supported");
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .context("invalid Content-Length")?
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        anyhow::bail!("HTTP body exceeds {MAX_BODY_BYTES} bytes");
    }
    while bytes.len() - header_end < content_length {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.context("read HTTP body")?;
        if read == 0 {
            anyhow::bail!("HTTP client disconnected before body completed");
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn origin_is_allowed(origin: Option<&str>) -> bool {
    origin.is_none_or(|origin| {
        let Some(authority) = origin.strip_prefix("http://") else {
            return false;
        };
        let Some((host, port)) = authority.rsplit_once(':') else {
            return false;
        };
        matches!(host, "127.0.0.1" | "localhost") && port.parse::<u16>().is_ok()
    })
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &[u8],
) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn write_empty_response(stream: &mut TcpStream, status: u16, reason: &str) -> Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n"
    );
    stream.write_all(head.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

fn hash_secret(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capability_binding_replaces_old_session_capability() {
        let registry = CapabilityRegistry::default();
        let old = registry.prepare(None).await;
        let session_id = acp::schema::v1::SessionId::new("session");
        assert!(registry.bind(&old, session_id.clone()).await);
        let new = registry.prepare(Some(session_id.clone())).await;
        assert!(registry.bind(&new, session_id.clone()).await);
        assert!(matches!(
            registry.resolve(&old.secret).await,
            CapabilityResolution::Unknown
        ));
        assert!(matches!(
            registry.resolve(&new.secret).await,
            CapabilityResolution::Bound(found) if found == session_id
        ));
    }

    #[tokio::test]
    async fn cancelled_replacement_preserves_committed_capability() {
        let registry = CapabilityRegistry::default();
        let session_id = acp::schema::v1::SessionId::new("session");
        let committed = registry.prepare(None).await;
        assert!(registry.bind(&committed, session_id.clone()).await);

        let replacement = registry.prepare(Some(session_id.clone())).await;
        registry.cancel(&replacement).await;

        assert!(matches!(
            registry.resolve(&committed.secret).await,
            CapabilityResolution::Bound(found) if found == session_id
        ));
        assert!(matches!(
            registry.resolve(&replacement.secret).await,
            CapabilityResolution::Unknown
        ));
    }

    #[tokio::test]
    async fn server_config_is_http_and_carries_only_its_session_capability() {
        let registry = CapabilityRegistry::default();
        let pending = registry.prepare(None).await;
        let config = server_config("http://127.0.0.1:4321/mcp", &pending);
        let acp::schema::v1::McpServer::Http(config) = config else {
            panic!("proposal MCP must use HTTP");
        };
        assert_eq!(config.name, "intelligent_terminal");
        assert_eq!(config.url, "http://127.0.0.1:4321/mcp");
        assert_eq!(config.headers.len(), 1);
        assert_eq!(config.headers[0].name, "Authorization");
        assert_eq!(
            config.headers[0].value.strip_prefix("Bearer "),
            Some(pending.secret.as_str())
        );
    }

    #[tokio::test]
    async fn request_reader_enforces_framing_and_body_length() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let client = async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\n{}")
                .await
                .unwrap();
        };
        let server = async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await.unwrap();
            assert_eq!(request.method, "POST");
            assert_eq!(request.path, "/mcp");
            assert_eq!(request.body, b"{}");
        };
        tokio::join!(client, server);
    }

    #[tokio::test]
    async fn request_reader_rejects_duplicate_headers() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let client = async move {
            let mut stream = TcpStream::connect(address).await.unwrap();
            stream
                .write_all(b"POST /mcp HTTP/1.1\r\nContent-Length: 0\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
        };
        let server = async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let error = match read_request(&mut stream).await {
                Ok(_) => panic!("duplicate headers must be rejected"),
                Err(error) => error,
            };
            assert!(error.to_string().contains("duplicate"));
        };
        tokio::join!(client, server);
    }

    #[test]
    fn origin_validation_accepts_only_loopback_origins() {
        assert!(origin_is_allowed(None));
        assert!(origin_is_allowed(Some("http://127.0.0.1:1234")));
        assert!(origin_is_allowed(Some("http://localhost:1234")));
        assert!(!origin_is_allowed(Some("https://example.com")));
        assert!(!origin_is_allowed(Some("http://localhost.example:1234")));
        assert!(!origin_is_allowed(Some("null")));
    }
}
