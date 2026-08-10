use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Duration;

use agent_client_protocol as acp;
use anyhow::{Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use uuid::Uuid;

use super::{AgentInstanceId, MasterStateInner};
use crate::agent_source::AgentSource;
use crate::agent_tools::action_proposal::mcp::{
    HelperRequest, SERVER_ID_HEX_LEN, SERVER_NAME_PREFIX,
};
use crate::agent_tools::action_proposal::pipe::ProposalValidationResponse;

const ENDPOINT_PATH: &str = "/mcp";
const MAX_HEADER_BYTES: usize = 32 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;
const HTTP_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const HELPER_TIMEOUT: Duration = Duration::from_secs(25);
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const WSL_RELAY_SCRIPT: &str = r#"
import base64
import os
import socket
import socketserver
import subprocess
import sys
import threading
import time

UPSTREAM_HOST = sys.argv[1]
UPSTREAM_PORT = sys.argv[2]
LISTEN_PORT = int(sys.argv[3])
MAX_CONNECTIONS = 32
READ_TIMEOUT_SECONDS = 5
POWERSHELL = r'''
__D__client = [Net.Sockets.TcpClient]::new()
__D__client.Connect('__WTA_HOST__', [int]'__WTA_PORT__')
__D__network = __D__client.GetStream()
__D__stdin = [Console]::OpenStandardInput()
__D__stdin.CopyTo(__D__network)
__D__client.Client.Shutdown([Net.Sockets.SocketShutdown]::Send)
__D__stdout = [Console]::OpenStandardOutput()
__D__network.CopyTo(__D__stdout)
'''
POWERSHELL = POWERSHELL.replace("__D__", chr(36))
POWERSHELL = POWERSHELL.replace("__WTA_HOST__", UPSTREAM_HOST)
POWERSHELL = POWERSHELL.replace("__WTA_PORT__", UPSTREAM_PORT)
POWERSHELL_ENCODED = base64.b64encode(POWERSHELL.encode("utf-16le")).decode("ascii")

def forward(request):
    process = subprocess.run(
        ["powershell.exe", "-NoLogo", "-NoProfile", "-NonInteractive",
         "-EncodedCommand", POWERSHELL_ENCODED],
        input=request, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        timeout=35, check=False)
    if process.returncode != 0:
        raise RuntimeError("Windows bridge exited with a nonzero status")
    return process.stdout

def send_response(sock, status, reason, message):
    body = message.encode("utf-8")
    response = (
        "HTTP/1.1 " + str(status) + " " + reason + "\r\n"
        "Content-Type: text/plain\r\n"
        "Content-Length: " + str(len(body)) + "\r\n"
        "Connection: close\r\n"
        "Cache-Control: no-store\r\n\r\n"
    ).encode("ascii") + body
    try:
        sock.sendall(response)
    except OSError:
        pass

def read_request(sock):
    data = b""
    while b"\r\n\r\n" not in data:
        chunk = sock.recv(4096)
        if not chunk:
            raise ValueError("client disconnected before HTTP headers")
        data += chunk
        if len(data) > 32768:
            raise ValueError("HTTP headers exceed the size limit")
    head, body = data.split(b"\r\n\r\n", 1)
    lines = head.split(b"\r\n")
    content_length = 0
    rewritten = [lines[0]]
    for line in lines[1:]:
        name, separator, value = line.partition(b":")
        if not separator:
            raise ValueError("malformed HTTP header")
        lower = name.strip().lower()
        if lower == b"content-length":
            content_length = int(value.strip())
            if content_length < 0 or content_length > 1048576:
                raise ValueError("HTTP body exceeds the size limit")
        if lower == b"host":
            line = b"Host: " + UPSTREAM_HOST.encode() + b":" + UPSTREAM_PORT.encode()
        elif lower == b"origin":
            origin = value.strip().lower()
            if origin.startswith(b"http://127.0.0.1:") or origin.startswith(b"http://localhost:"):
                line = b"Origin: http://" + UPSTREAM_HOST.encode() + b":" + UPSTREAM_PORT.encode()
        rewritten.append(line)
    while len(body) < content_length:
        chunk = sock.recv(min(4096, content_length - len(body)))
        if not chunk:
            raise ValueError("client disconnected before HTTP body")
        body += chunk
    return b"\r\n".join(rewritten) + b"\r\n\r\n" + body[:content_length]

class Handler(socketserver.BaseRequestHandler):
    def handle(self):
        self.request.settimeout(READ_TIMEOUT_SECONDS)
        try:
            request = read_request(self.request)
        except socket.timeout:
            send_response(self.request, 408, "Request Timeout", "request timed out")
            return
        except (OSError, ValueError):
            send_response(self.request, 400, "Bad Request", "invalid HTTP request")
            return
        try:
            response = forward(request)
        except subprocess.TimeoutExpired:
            send_response(self.request, 504, "Gateway Timeout", "Windows bridge timed out")
            return
        except (OSError, RuntimeError):
            send_response(self.request, 502, "Bad Gateway", "Windows bridge failed")
            return
        self.request.sendall(response)

class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = False
    daemon_threads = True
    request_queue_size = MAX_CONNECTIONS

    def __init__(self, server_address, handler):
        self.connection_slots = threading.BoundedSemaphore(MAX_CONNECTIONS)
        super().__init__(server_address, handler)

    def process_request(self, request, client_address):
        if not self.connection_slots.acquire(blocking=False):
            send_response(request, 503, "Service Unavailable", "relay is busy")
            self.shutdown_request(request)
            return
        try:
            super().process_request(request, client_address)
        except Exception:
            self.connection_slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.connection_slots.release()

def exit_when_owner_disconnects():
    try:
        sys.stdin.buffer.read()
    finally:
        os._exit(0)

with Server(("127.0.0.1", LISTEN_PORT), Handler) as server:
    threading.Thread(target=exit_when_owner_disconnects, daemon=True).start()
    probe = ("GET /mcp HTTP/1.1\r\nHost: " + UPSTREAM_HOST + ":" +
             UPSTREAM_PORT + "\r\nConnection: close\r\n\r\n").encode()
    for attempt in range(50):
        try:
            response = forward(probe)
        except (OSError, RuntimeError, subprocess.TimeoutExpired):
            response = b""
        if response.startswith(b"HTTP/1.1 401"):
            break
        time.sleep(0.1)
    else:
        raise RuntimeError("Windows loopback bridge is unavailable")
    print(server.server_address[1], flush=True)
    server.serve_forever()
"#;

pub(super) struct Endpoints {
    host: String,
    wsl: Mutex<HashMap<String, Arc<Mutex<RelaySlot>>>>,
}

struct WslRelay {
    endpoint: String,
    port: u16,
    child: tokio::process::Child,
}

#[derive(Default)]
struct RelaySlot {
    relay: Option<WslRelay>,
    port: Option<u16>,
    supervisor_started: bool,
}

impl Endpoints {
    pub(super) fn new(host: String) -> Self {
        Self {
            host,
            wsl: Mutex::new(HashMap::new()),
        }
    }
}

#[derive(Clone)]
pub(super) struct PendingCapability {
    secret: String,
    hash: [u8; 32],
    server_name: String,
}

#[derive(Default)]
pub(super) struct CapabilityRegistry {
    routes: Mutex<CapabilityRoutes>,
}

#[derive(Default)]
struct CapabilityRoutes {
    by_capability: HashMap<[u8; 32], CapabilityRoute>,
    by_session: HashMap<acp::schema::v1::SessionId, [u8; 32]>,
    by_owner: HashMap<AgentInstanceId, HashSet<[u8; 32]>>,
}

struct CapabilityRoute {
    session_id: Option<acp::schema::v1::SessionId>,
    owner: AgentInstanceId,
}

impl CapabilityRegistry {
    pub(super) async fn prepare(
        &self,
        owner: AgentInstanceId,
        session_id: Option<acp::schema::v1::SessionId>,
    ) -> PendingCapability {
        let mut server_id = Uuid::new_v4().simple().to_string();
        server_id.truncate(SERVER_ID_HEX_LEN);
        let server_name = format!("{SERVER_NAME_PREFIX}{server_id}");
        let secret = Uuid::new_v4().simple().to_string();
        let hash = hash_secret(&secret);
        let mut routes = self.routes.lock().await;
        routes
            .by_capability
            .insert(hash, CapabilityRoute { session_id, owner });
        routes.by_owner.entry(owner).or_default().insert(hash);
        PendingCapability {
            secret,
            hash,
            server_name,
        }
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
            if old != pending.hash {
                Self::remove_capability(&mut routes, &old);
            }
        }
        if let Some(route) = routes.by_capability.get_mut(&pending.hash) {
            route.session_id = Some(session_id);
        }
        true
    }

    pub(super) async fn cancel(&self, pending: &PendingCapability) {
        let mut routes = self.routes.lock().await;
        Self::remove_capability(&mut routes, &pending.hash);
    }

    pub(super) async fn remove_session(&self, session_id: &acp::schema::v1::SessionId) {
        let mut routes = self.routes.lock().await;
        if let Some(hash) = routes.by_session.get(session_id).copied() {
            Self::remove_capability(&mut routes, &hash);
        }
    }

    pub(super) async fn remove_owner(&self, owner: AgentInstanceId) -> usize {
        let mut routes = self.routes.lock().await;
        let Some(hashes) = routes.by_owner.remove(&owner) else {
            return 0;
        };
        let count = hashes.len();
        for hash in hashes {
            if let Some(route) = routes.by_capability.remove(&hash) {
                if let Some(session_id) = route.session_id {
                    if routes.by_session.get(&session_id) == Some(&hash) {
                        routes.by_session.remove(&session_id);
                    }
                }
            }
        }
        count
    }

    async fn resolve(&self, secret: &str) -> CapabilityResolution {
        match self
            .routes
            .lock()
            .await
            .by_capability
            .get(&hash_secret(secret))
            .map(|route| route.session_id.clone())
        {
            Some(Some(session_id)) => CapabilityResolution::Bound(session_id),
            Some(None) => CapabilityResolution::Pending,
            None => CapabilityResolution::Unknown,
        }
    }

    fn remove_capability(routes: &mut CapabilityRoutes, hash: &[u8; 32]) {
        let Some(route) = routes.by_capability.remove(hash) else {
            return;
        };
        let remove_owner = if let Some(hashes) = routes.by_owner.get_mut(&route.owner) {
            hashes.remove(hash);
            hashes.is_empty()
        } else {
            false
        };
        if remove_owner {
            routes.by_owner.remove(&route.owner);
        }
        if let Some(session_id) = route.session_id {
            if routes.by_session.get(&session_id) == Some(hash) {
                routes.by_session.remove(&session_id);
            }
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
        acp::schema::v1::McpServerHttp::new(pending.server_name.clone(), endpoint).headers(vec![
            acp::schema::v1::HttpHeader::new("Authorization", format!("Bearer {}", pending.secret)),
        ]),
    )
}

pub(super) async fn endpoint_for(
    state: &Arc<MasterStateInner>,
    source: &AgentSource,
) -> Result<String> {
    let AgentSource::Wsl { distro } = source else {
        return Ok(state.proposal_mcp_endpoints.host.clone());
    };

    let relay_slot = {
        let mut relays = state.proposal_mcp_endpoints.wsl.lock().await;
        Arc::clone(
            relays
                .entry(distro.clone())
                .or_insert_with(|| Arc::new(Mutex::new(RelaySlot::default()))),
        )
    };
    let mut slot = relay_slot.lock().await;
    if let Some(relay) = slot.relay.as_mut() {
        if relay.child.try_wait()?.is_none() {
            return Ok(relay.endpoint.clone());
        }
        slot.relay = None;
    }

    let upstream = state
        .proposal_mcp_endpoints
        .host
        .strip_prefix("http://")
        .and_then(|value| value.strip_suffix(ENDPOINT_PATH))
        .context("proposal MCP host endpoint is malformed")?;
    let (upstream_host, upstream_port) = upstream
        .rsplit_once(':')
        .context("proposal MCP host endpoint has no port")?;
    let relay = start_wsl_relay(
        distro,
        upstream_host,
        upstream_port,
        slot.port.unwrap_or(0),
    )
    .await?;
    let endpoint = relay.endpoint.clone();
    slot.port = Some(relay.port);
    slot.relay = Some(relay);
    let start_supervisor = !slot.supervisor_started;
    slot.supervisor_started = true;
    drop(slot);

    if start_supervisor {
        spawn_wsl_relay_supervisor(
            Arc::downgrade(&relay_slot),
            distro.clone(),
            upstream_host.to_string(),
            upstream_port.to_string(),
        );
    }
    Ok(endpoint)
}

async fn start_wsl_relay(
    distro: &str,
    upstream_host: &str,
    upstream_port: &str,
    listen_port: u16,
) -> Result<WslRelay> {
    let mut command = tokio::process::Command::new("wsl.exe");
    command
        .arg("-d")
        .arg(distro)
        .arg("--")
        .arg("python3")
        .arg("-u")
        .arg("-c")
        .arg(WSL_RELAY_SCRIPT)
        .arg(upstream_host)
        .arg(upstream_port)
        .arg(listen_port.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = command
        .spawn()
        .with_context(|| format!("start proposal MCP relay in WSL distro {distro}"))?;
    let stdout = child
        .stdout
        .take()
        .context("WSL proposal MCP relay has no stdout")?;
    let mut stdout = tokio::io::BufReader::new(stdout);
    let mut port = String::new();
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::io::AsyncBufReadExt::read_line(&mut stdout, &mut port),
    )
    .await
    .with_context(|| format!("timed out starting proposal MCP relay in {distro}"))?
    .with_context(|| format!("read proposal MCP relay port from {distro}"))?;
    let port = port
        .trim()
        .parse::<u16>()
        .with_context(|| format!("invalid proposal MCP relay port from {distro}"))?;
    if child.try_wait()?.is_some() {
        anyhow::bail!("proposal MCP relay exited during startup in {distro}");
    }
    let endpoint = format!("http://127.0.0.1:{port}{ENDPOINT_PATH}");
    tracing::info!(
        target: "proposal_mcp",
        distro,
        endpoint = %endpoint,
        "WSL proposal MCP loopback relay ready"
    );
    Ok(WslRelay {
        endpoint,
        port,
        child,
    })
}

fn spawn_wsl_relay_supervisor(
    relay_slot: Weak<Mutex<RelaySlot>>,
    distro: String,
    upstream_host: String,
    upstream_port: String,
) {
    tokio::task::spawn_local(async move {
        let mut retry_delay = Duration::from_secs(1);
        loop {
            tokio::time::sleep(retry_delay).await;
            let Some(relay_slot) = relay_slot.upgrade() else {
                return;
            };
            let mut slot = relay_slot.lock().await;
            let relay_stopped = match slot.relay.as_mut() {
                Some(relay) => match relay.child.try_wait() {
                    Ok(None) => {
                        retry_delay = Duration::from_secs(1);
                        continue;
                    }
                    Ok(Some(status)) => {
                        tracing::warn!(
                            target: "proposal_mcp",
                            distro = %distro,
                            ?status,
                            "WSL proposal MCP relay exited; restarting on the same port"
                        );
                        true
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "proposal_mcp",
                            distro = %distro,
                            %error,
                            "failed to inspect WSL proposal MCP relay; restarting on the same port"
                        );
                        true
                    }
                },
                None => true,
            };
            if !relay_stopped {
                continue;
            }
            slot.relay = None;
            let Some(port) = slot.port else {
                return;
            };
            match start_wsl_relay(&distro, &upstream_host, &upstream_port, port).await {
                Ok(relay) => {
                    slot.relay = Some(relay);
                    retry_delay = Duration::from_secs(1);
                    tracing::info!(
                        target: "proposal_mcp",
                        distro = %distro,
                        port,
                        "WSL proposal MCP relay restarted"
                    );
                }
                Err(error) => {
                    retry_delay = (retry_delay * 2).min(Duration::from_secs(30));
                    tracing::warn!(
                        target: "proposal_mcp",
                        distro = %distro,
                        port,
                        retry_secs = retry_delay.as_secs(),
                        error = %format!("{error:#}"),
                        "failed to restart WSL proposal MCP relay"
                    );
                }
            }
        }
    });
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
            if message.get("method").and_then(Value::as_str) == Some("tools/call") {
                let session_id = match &capability {
                    CapabilityResolution::Bound(session_id) => Some(session_id.to_string()),
                    CapabilityResolution::Pending | CapabilityResolution::Unknown => None,
                };
                tracing::info!(
                    target: "proposal_mcp",
                    step = "agent→master",
                    op = "tools/call",
                    session_id = session_id.as_deref(),
                    capability_bound = session_id.is_some(),
                    "received terminal action MCP call"
                );
            }
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
    let started = std::time::Instant::now();
    let session_id = match capability {
        CapabilityResolution::Bound(session_id) => session_id,
        CapabilityResolution::Pending => {
            tracing::warn!(
                target: "proposal_mcp",
                step = "master→helper",
                op = "request_terminal_actions",
                stage = "resolve_capability",
                "terminal action MCP call rejected because its ACP session is not bound"
            );
            anyhow::bail!("ACP session is not bound yet");
        }
        CapabilityResolution::Unknown => {
            tracing::warn!(
                target: "proposal_mcp",
                step = "master→helper",
                op = "request_terminal_actions",
                stage = "resolve_capability",
                "terminal action MCP call rejected because its capability is unknown"
            );
            anyhow::bail!("MCP capability is unknown");
        }
    };
    let route = {
        let routes = state.session_to_helper.lock().await;
        routes.get(&session_id).cloned()
    };
    let Some(route) = route else {
        tracing::warn!(
            target: "proposal_mcp",
            step = "master→helper",
            op = "request_terminal_actions",
            stage = "resolve_helper",
            session_id = %session_id,
            "terminal action MCP call rejected because its owning Helper is disconnected"
        );
        anyhow::bail!("owning Helper is disconnected");
    };
    let Some(forwarder) = route.forwarder else {
        tracing::error!(
            target: "proposal_mcp",
            step = "master→helper",
            op = "request_terminal_actions",
            stage = "resolve_helper",
            helper_id = ?route.helper_id,
            session_id = %session_id,
            "terminal action MCP route has no Helper forwarder"
        );
        anyhow::bail!("owning Helper route has no forwarder");
    };
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
        step = "master→helper",
        op = "request_terminal_actions",
        helper_id = ?route.helper_id,
        session_id = %session_id,
        "routing terminal action request to owning Helper"
    );
    let response = match tokio::time::timeout(HELPER_TIMEOUT, forwarder.ext_method(request)).await {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            tracing::warn!(
                target: "proposal_mcp",
                step = "helper→master",
                op = "request_terminal_actions",
                stage = "helper_rpc",
                helper_id = ?route.helper_id,
                session_id = %session_id,
                elapsed_ms = started.elapsed().as_millis() as u64,
                error_code = ?error.code,
                "owning Helper rejected terminal action request"
            );
            return Err(anyhow::Error::new(error)
                .context("owning Helper rejected terminal action request"));
        }
        Err(_) => {
            tracing::warn!(
                target: "proposal_mcp",
                step = "helper→master",
                op = "request_terminal_actions",
                stage = "helper_rpc",
                helper_id = ?route.helper_id,
                session_id = %session_id,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "timed out waiting for owning Helper"
            );
            anyhow::bail!("timed out waiting for owning Helper");
        }
    };
    let response: ProposalValidationResponse = match serde_json::from_str(response.0.get()) {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(
                target: "proposal_mcp",
                step = "helper→master",
                op = "request_terminal_actions",
                stage = "decode_response",
                helper_id = ?route.helper_id,
                session_id = %session_id,
                elapsed_ms = started.elapsed().as_millis() as u64,
                "failed to decode Helper terminal action response"
            );
            return Err(error).context("decode Helper proposal response");
        }
    };
    tracing::info!(
        target: "proposal_mcp",
        step = "helper→master",
        op = "request_terminal_actions",
        helper_id = ?route.helper_id,
        session_id = %session_id,
        status = ?response.status,
        retryable = response.retryable,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "terminal action MCP call completed"
    );
    Ok(response)
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
        let owner = AgentInstanceId::new_v4();
        let old = registry.prepare(owner, None).await;
        let session_id = acp::schema::v1::SessionId::new("session");
        assert!(registry.bind(&old, session_id.clone()).await);
        let new = registry.prepare(owner, Some(session_id.clone())).await;
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
        let owner = AgentInstanceId::new_v4();
        let session_id = acp::schema::v1::SessionId::new("session");
        let committed = registry.prepare(owner, None).await;
        assert!(registry.bind(&committed, session_id.clone()).await);

        let replacement = registry.prepare(owner, Some(session_id.clone())).await;
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
    async fn closing_session_revokes_committed_capability_but_not_replacement() {
        let registry = CapabilityRegistry::default();
        let owner = AgentInstanceId::new_v4();
        let session_id = acp::schema::v1::SessionId::new("session");
        let committed = registry.prepare(owner, None).await;
        assert!(registry.bind(&committed, session_id.clone()).await);
        let replacement = registry.prepare(owner, Some(session_id.clone())).await;

        registry.remove_session(&session_id).await;

        assert!(matches!(
            registry.resolve(&committed.secret).await,
            CapabilityResolution::Unknown
        ));
        assert!(matches!(
            registry.resolve(&replacement.secret).await,
            CapabilityResolution::Bound(found) if found == session_id
        ));
    }

    #[tokio::test]
    async fn server_configs_isolate_session_identity_and_capability() {
        let registry = CapabilityRegistry::default();
        let pending = registry.prepare(AgentInstanceId::new_v4(), None).await;
        let other = registry.prepare(AgentInstanceId::new_v4(), None).await;
        let config = server_config("http://127.0.0.1:4321/mcp", &pending);
        let acp::schema::v1::McpServer::Http(config) = config else {
            panic!("proposal MCP must use HTTP");
        };
        let other_config = server_config("http://127.0.0.1:4321/mcp", &other);
        let acp::schema::v1::McpServer::Http(other_config) = other_config else {
            panic!("proposal MCP must use HTTP");
        };
        let repeated = server_config("http://127.0.0.1:4321/mcp", &pending);
        let acp::schema::v1::McpServer::Http(repeated) = repeated else {
            panic!("proposal MCP must use HTTP");
        };
        assert_ne!(config.name, other_config.name);
        assert_eq!(config.name, repeated.name);
        let server_id = config
            .name
            .strip_prefix("intellterm_")
            .expect("proposal MCP server name must use the reserved prefix");
        assert_eq!(server_id.len(), 20);
        assert!(
            server_id
                .chars()
                .all(|ch| ch.is_ascii_digit() || ('a'..='f').contains(&ch))
        );
        assert!(format!("mcp__{}__request_terminal_actions", config.name).len() <= 64);
        assert!(!config.name.contains(&pending.secret));
        assert_eq!(config.url, "http://127.0.0.1:4321/mcp");
        assert_eq!(config.headers.len(), 1);
        assert_eq!(config.headers[0].name, "Authorization");
        assert_eq!(
            config.headers[0].value.strip_prefix("Bearer "),
            Some(pending.secret.as_str())
        );
    }

    #[tokio::test]
    async fn removing_agent_owner_revokes_only_its_capabilities() {
        let registry = CapabilityRegistry::default();
        let removed_owner = AgentInstanceId::new_v4();
        let retained_owner = AgentInstanceId::new_v4();
        let removed = registry.prepare(removed_owner, None).await;
        let retained = registry.prepare(retained_owner, None).await;
        assert!(
            registry
                .bind(
                    &removed,
                    acp::schema::v1::SessionId::new("removed-session")
                )
                .await
        );
        assert!(
            registry
                .bind(
                    &retained,
                    acp::schema::v1::SessionId::new("retained-session")
                )
                .await
        );

        assert_eq!(registry.remove_owner(removed_owner).await, 1);
        assert!(matches!(
            registry.resolve(&removed.secret).await,
            CapabilityResolution::Unknown
        ));
        assert!(matches!(
            registry.resolve(&retained.secret).await,
            CapabilityResolution::Bound(session_id)
                if session_id == acp::schema::v1::SessionId::new("retained-session")
        ));
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

    #[test]
    fn wsl_relay_script_survives_wsl_interop_argument_expansion() {
        assert!(
            !WSL_RELAY_SCRIPT.contains('$'),
            "wsl.exe expands dollar expressions before Python receives -c"
        );
        assert!(WSL_RELAY_SCRIPT.contains("chr(36)"));
        assert!(WSL_RELAY_SCRIPT.contains("-EncodedCommand"));
        assert!(WSL_RELAY_SCRIPT.contains("sys.stdin.buffer.read()"));
        assert!(WSL_RELAY_SCRIPT.contains("BoundedSemaphore(MAX_CONNECTIONS)"));
        assert!(WSL_RELAY_SCRIPT.contains("settimeout(READ_TIMEOUT_SECONDS)"));
        assert!(WSL_RELAY_SCRIPT.contains("LISTEN_PORT = int(sys.argv[3])"));
    }
}
