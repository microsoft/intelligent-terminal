// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer};
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use super::engine::{Effect, Engine};
use super::wire::{Principal, Request, Response};
use crate::agent_tools::action_proposal::pipe_security;

pub(crate) const MAX_FRAME_BYTES: usize = 1_048_576;
const MAX_QUEUED_FRAMES: usize = 256;
const MAX_QUEUED_BYTES: usize = 8 * 1024 * 1024;
const RESERVED_CONTROL_FRAMES: usize = 8;
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

enum EngineMessage {
    Shutdown(oneshot::Sender<()>),
    Request(Principal, Request, oneshot::Sender<Response>),
    Complete(String, Response, oneshot::Sender<Result<()>>),
    Effects(oneshot::Sender<Result<Vec<Effect>>>),
    Snapshot(Value, oneshot::Sender<Result<(Value, String), Response>>),
    Events(
        String,
        Value,
        oneshot::Sender<Result<(Vec<Value>, String), Response>>,
    ),
}

#[derive(Clone)]
pub(crate) struct ServiceHandle {
    sender: mpsc::Sender<EngineMessage>,
    changes: watch::Receiver<u64>,
    store_id: String,
}

impl ServiceHandle {
    pub(super) fn watch_changes(&self) -> watch::Receiver<u64> {
        self.changes.clone()
    }

    pub(super) async fn shutdown(&self) -> Result<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(EngineMessage::Shutdown(sender))
            .await
            .map_err(|_| anyhow!("work service stopped before shutdown"))?;
        receiver.await.context("work store did not close cleanly")
    }

    pub(crate) async fn wait_operation(&self, principal: Principal, id: &str) -> Response {
        let mut changes = self.changes.clone();
        loop {
            // Subscribe before reading, so completion cannot strand the MCP call.
            changes.borrow_and_update();
            let response = self
                .request(
                    principal.clone(),
                    Request::new("operation.get", json!({"operationId":id})),
                )
                .await;
            if response.status != "ok" {
                return response;
            }
            let Some(view) = response.data.as_ref() else {
                return Response::fail(
                    response.request_id,
                    "PROTOCOL_INCOMPLETE",
                    "Operation view is missing",
                );
            };
            let operation = &view["operation"];
            match operation["status"].as_str() {
                Some("Succeeded") => {
                    let Some(result) = operation.get("result") else {
                        return Response::fail(
                            response.request_id,
                            "PROTOCOL_INCOMPLETE",
                            "Completed operation has no result receipt",
                        );
                    };
                    return Response::ok(response.request_id, result.clone());
                }
                Some("Failed" | "RepairRequired") => {
                    let mut error = Response::fail(
                        response.request_id,
                        if operation["status"] == "RepairRequired" {
                            "OUTCOME_UNKNOWN"
                        } else {
                            "EXECUTION_FAILED"
                        },
                        "The recorded operation did not complete; inspect its retained evidence",
                    );
                    error.operation_id = Some(id.to_owned());
                    if let Some(recorded) = view.get("failure").or_else(|| operation.get("failure"))
                    {
                        match serde_json::from_value(recorded.clone()) {
                            Ok(failure) => error.failure = Some(failure),
                            Err(decode) => {
                                tracing::error!(target:"agent_center", operation_id=id, %decode, "invalid stored operation failure")
                            }
                        }
                    }
                    return error;
                }
                Some("Pending" | "Running") => {}
                _ => {
                    return Response::fail(
                        response.request_id,
                        "PROTOCOL_INCOMPLETE",
                        "Operation has an invalid state",
                    )
                }
            }
            if changes.changed().await.is_err() {
                return service_stopped();
            }
        }
    }

    pub(crate) async fn request(&self, principal: Principal, request: Request) -> Response {
        let request_id = request.request_id.clone();
        let (sender, receiver) = oneshot::channel();
        if self
            .sender
            .send(EngineMessage::Request(principal, request, sender))
            .await
            .is_err()
        {
            return failure(
                &request_id,
                "error",
                "EXECUTION_FAILED",
                "The work service is unavailable",
            );
        }
        receiver.await.unwrap_or_else(|_| {
            failure(
                &request_id,
                "error",
                "EXECUTION_FAILED",
                "The work service stopped before replying",
            )
        })
    }

    pub(crate) async fn complete_effect(&self, id: &str, response: Response) -> Result<()> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(EngineMessage::Complete(id.to_owned(), response, sender))
            .await
            .map_err(|_| anyhow!("work service stopped while recording an effect"))?;
        receiver
            .await
            .context("work service lost effect completion")?
    }

    pub(super) async fn effects(&self) -> Result<Vec<Effect>> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(EngineMessage::Effects(sender))
            .await
            .map_err(|_| anyhow!("work service stopped while dispatching effects"))?;
        receiver
            .await
            .context("work service lost effect dispatch")?
    }

    async fn snapshot(&self, scope: Value) -> Result<(Value, String), Response> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(EngineMessage::Snapshot(scope, sender))
            .await
            .map_err(|_| service_stopped())?;
        receiver.await.map_err(|_| service_stopped())?
    }

    async fn events(&self, after: String, scope: Value) -> Result<(Vec<Value>, String), Response> {
        let (sender, receiver) = oneshot::channel();
        self.sender
            .send(EngineMessage::Events(after, scope, sender))
            .await
            .map_err(|_| service_stopped())?;
        receiver.await.map_err(|_| service_stopped())?
    }
}

fn service_stopped() -> Response {
    failure("", "error", "EXECUTION_FAILED", "The work service stopped")
}

fn failure(request_id: &str, status: &str, code: &str, message: &str) -> Response {
    let response = Response::fail(request_id, code, message);
    debug_assert_eq!(response.status, status);
    response
}

fn ok(request_id: &str, data: Value, cursor: Option<&str>) -> Result<Response> {
    let mut response =
        json!({"type":"response","requestId":request_id,"status":"ok","data":data,"subjects":[]});
    if let Some(cursor) = cursor {
        response["cursor"] = json!(cursor);
    }
    serde_json::from_value(response).context("constructing service response")
}

pub(crate) fn state_root() -> Result<PathBuf> {
    resolve_state_root(
        std::env::var_os("INTELLIGENT_TERMINAL_AGENT_CENTER_STATE").map(PathBuf::from),
        crate::runtime_paths::intelligent_terminal_root(),
    )
}

fn resolve_state_root(explicit: Option<PathBuf>, application: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(root) = explicit {
        anyhow::ensure!(
            root.is_absolute(),
            "INTELLIGENT_TERMINAL_AGENT_CENTER_STATE must be an absolute directory"
        );
        return Ok(root);
    }
    application
        .map(|root| root.join("agent-center"))
        .context("Agent Center requires an available application state directory")
}

pub(crate) async fn configure(input: &Path) -> Result<()> {
    configure_at(input, &state_root()?).await?;
    println!(
        "{}",
        json!({"type":"response","requestId":Uuid::new_v4().to_string(),
        "status":"ok","subjects":[],"data":{"configured":true,"effectiveOnNextServiceStart":true}})
    );
    Ok(())
}

async fn configure_at(input: &Path, root: &Path) -> Result<()> {
    let mut bytes = Vec::new();
    tokio::fs::File::open(input)
        .await
        .context("opening ACP adapter configuration")?
        .take(MAX_FRAME_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .context("reading ACP adapter configuration")?;
    if bytes.len() > MAX_FRAME_BYTES {
        bail!("ACP adapter configuration exceeds 1 MiB");
    }
    super::runtime::validate_adapter_configuration(&bytes)?;
    tokio::fs::create_dir_all(root).await?;
    let _authority = lock_authority(root)
        .context("Configure adapters before starting Agent Center; a running authority cannot be reconfigured implicitly")?;
    let staging = root.join(format!("adapters-{}.staging", Uuid::new_v4()));
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&staging)
        .await?;
    let result: Result<()> = async {
        file.write_all(&bytes).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&staging, root.join("adapters.json"))
            .await
            .context("installing ACP adapter configuration")?;
        Ok(())
    }
    .await;
    if result.is_err() {
        if let Err(error) = tokio::fs::remove_file(&staging).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(target:"agent_center", %error, "adapter staging file needs cleanup");
            }
        }
    }
    result
}

fn pipe_name(root: &Path) -> Result<String> {
    let canonical = root
        .canonicalize()
        .context("resolving Agent Center state directory")?;
    let digest = Sha256::digest(
        canonical
            .as_os_str()
            .to_string_lossy()
            .to_lowercase()
            .as_bytes(),
    );
    Ok(format!(
        r"\\.\pipe\IntelligentTerminal-AgentCenter-{:x}",
        digest
    ))
}

fn create_pipe(name: &str, first: bool) -> Result<NamedPipeServer> {
    let security = pipe_security::build_required()?;
    pipe_security::create_server(name, first, Some(&security))
        .context("creating private Agent Center pipe")
}

fn lock_authority(root: &Path) -> Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .share_mode(0)
        .open(root.join("authority.lock"))
        .context("Agent Center already has an authority, or its state directory cannot be locked")
}

pub(super) async fn start_engine(root: PathBuf) -> Result<ServiceHandle> {
    let engine = tokio::task::spawn_blocking(move || Engine::open(&root)).await??;
    let store_id = engine.store_id().to_owned();
    let (sender, mut receiver) = mpsc::channel::<EngineMessage>(128);
    let (changes_sender, changes) = watch::channel(0_u64);
    tokio::task::spawn_blocking(move || {
        let mut engine = engine;
        while let Some(message) = receiver.blocking_recv() {
            let before = engine.cursor();
            let effect_completion = matches!(&message, EngineMessage::Complete(..));
            match message {
                EngineMessage::Shutdown(reply) => {
                    drop(engine);
                    let _ = reply.send(());
                    break;
                }
                EngineMessage::Request(principal, request, reply) => {
                    // A disconnected caller does not undo an already committed command.
                    let _ = reply.send(engine.handle(&principal, request));
                }
                EngineMessage::Complete(id, response, reply) => {
                    let result = engine.complete_effect(&id, response);
                    if let Err(error) = &result {
                        tracing::error!(target:"agent_center", effect_id = id, %error, "effect receipt could not be recorded");
                    }
                    let _ = reply.send(result);
                }
                EngineMessage::Effects(reply) => {
                    let _ = reply.send(engine.take_effects());
                }
                EngineMessage::Snapshot(scope, reply) => {
                    let result = engine
                        .snapshot(&scope)
                        .map(|snapshot| (snapshot, engine.cursor()));
                    let _ = reply.send(result);
                }
                EngineMessage::Events(after, scope, reply) => {
                    let result = engine
                        .events_after(Some(&after), &scope)
                        .map(|events| (events, engine.cursor()));
                    let _ = reply.send(result);
                }
            }
            if effect_completion || before != engine.cursor() {
                changes_sender.send_modify(|generation| *generation = generation.wrapping_add(1));
            }
        }
    });
    Ok(ServiceHandle {
        sender,
        changes,
        store_id,
    })
}

pub(crate) async fn serve() -> Result<()> {
    let root = state_root()?;
    tokio::fs::create_dir_all(&root)
        .await
        .context("creating Agent Center state directory")?;
    let _authority = lock_authority(&root)?;
    let name = pipe_name(&root)?;
    let mut listener = create_pipe(&name, true)?;
    let handle = start_engine(root.clone()).await?;
    let runtime = super::runtime::Runtime::new(handle.clone(), root)?;
    runtime.register().await?;
    let dispatch_runtime = runtime.clone();
    let service_instance = Uuid::new_v4().to_string();
    let dispatch_handle = handle.clone();
    let mut dispatcher = tokio::spawn(async move {
        let mut changes = dispatch_handle.watch_changes();
        loop {
            for effect in dispatch_handle.effects().await? {
                let runtime = dispatch_runtime.clone();
                tokio::spawn(async move {
                    let id = effect.id.clone();
                    if let Err(error) = runtime.execute(effect).await {
                        tracing::error!(target:"agent_center", effect_id = id, %error, "effect requires reconciliation");
                    }
                });
            }
            changes
                .changed()
                .await
                .context("work engine event dispatcher stopped")?;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    });
    let result = loop {
        tokio::select! {
            result = listener.connect() => {
                if let Err(error) = result { break Err(error.into()); }
                let connected = listener;
                listener = match create_pipe(&name, false) {
                    Ok(listener) => listener,
                    Err(error) => break Err(error),
                };
                let service = handle.clone();
                let instance = service_instance.clone();
                tokio::spawn(async move {
                    if let Err(error) = connection(connected, service, instance).await {
                        tracing::warn!(target:"agent_center", %error, "Console connection ended");
                    }
                });
            }
            result = &mut dispatcher => {
                break match result {
                    Ok(Err(error)) => Err(error),
                    Ok(Ok(())) => Err(anyhow!("Agent Center dispatcher stopped unexpectedly")),
                    Err(error) => Err(error.into()),
                };
            }
            result = tokio::signal::ctrl_c() => { break result.context("waiting for service interruption"); }
        }
    };
    dispatcher.abort();
    let runtime_shutdown = runtime.shutdown().await;
    handle.shutdown().await?;
    runtime_shutdown?;
    result
}

async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Value> {
    let length = reader.read_u32_le().await.context("reading frame length")? as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        bail!("INVALID_FRAME: payload length {length} is outside 1..={MAX_FRAME_BYTES}");
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .context("reading frame payload")?;
    serde_json::from_slice(&bytes)
        .context("INVALID_FRAME: payload is not a JSON object")
        .and_then(|value: Value| {
            if value.is_object() {
                Ok(value)
            } else {
                bail!("INVALID_FRAME: expected a JSON object")
            }
        })
}

async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        bail!("INVALID_FRAME: response exceeds the 1 MiB frame limit; use an artifact reference");
    }
    writer.write_u32_le(bytes.len() as u32).await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Hello {
    #[serde(rename = "type")]
    kind: String,
    versions: Vec<u32>,
    client_instance_id: String,
    client_kind: String,
}

struct Outbound {
    bytes: Vec<u8>,
    _permit: OwnedSemaphorePermit,
}

fn queue(sender: &mpsc::Sender<Outbound>, budget: &Arc<Semaphore>, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        bail!("response exceeds maximum frame size");
    }
    let permit = budget
        .clone()
        .try_acquire_many_owned(bytes.len() as u32)
        .context("RESYNC_REQUIRED: outbound byte budget exceeded")?;
    sender
        .try_send(Outbound {
            bytes,
            _permit: permit,
        })
        .map_err(|_| anyhow!("RESYNC_REQUIRED: outbound frame queue exceeded or closed"))
}

async fn queue_control(
    sender: &mpsc::Sender<Outbound>,
    budget: &Arc<Semaphore>,
    value: &Value,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_BYTES {
        bail!("response exceeds maximum frame size");
    }
    tokio::time::timeout(REQUEST_TIMEOUT, async {
        let permit = budget
            .clone()
            .acquire_many_owned(bytes.len() as u32)
            .await?;
        sender
            .send(Outbound {
                bytes,
                _permit: permit,
            })
            .await
            .map_err(|_| anyhow!("outbound writer closed"))
    })
    .await
    .context("OUTBOUND_TIMEOUT: peer stopped reading replies or recovery diagnostics")?
}

struct Subscription {
    scope: Value,
    cursor: String,
}

async fn negotiate<S: AsyncRead + AsyncWrite + Unpin>(
    pipe: &mut S,
    store_id: &str,
    instance: &str,
) -> Result<Option<Hello>> {
    let hello = match read_frame(pipe)
        .await
        .and_then(|v| serde_json::from_value::<Hello>(v).map_err(Into::into))
    {
        Ok(hello)
            if hello.kind == "hello"
                && Uuid::parse_str(&hello.client_instance_id).is_ok()
                && ["CLI", "Console", "Runtime"].contains(&hello.client_kind.as_str()) =>
        {
            hello
        }
        result => {
            write_frame(pipe, &json!({"type":"protocol_error","code":"INVALID_FRAME","message":"The first frame must be a valid hello"})).await?;
            return match result {
                Err(error) => Err(error),
                _ => Err(anyhow!("invalid hello")),
            };
        }
    };
    if !hello.versions.contains(&1) {
        write_frame(pipe, &json!({"type":"protocol_error","code":"INCOMPATIBLE_VERSION","message":"Agent Center requires protocol v1","supportedVersions":[1]})).await?;
        return Ok(None);
    }
    write_frame(
        pipe,
        &json!({
            "type":"welcome","version":1,"connectionId":Uuid::new_v4().to_string(),
            "serviceInstanceId":instance,"storeId":store_id,"maxFrameBytes":MAX_FRAME_BYTES
        }),
    )
    .await?;
    Ok(Some(hello))
}

async fn connection<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut pipe: S,
    handle: ServiceHandle,
    instance: String,
) -> Result<()> {
    let Some(hello) = negotiate(&mut pipe, &handle.store_id, &instance).await? else {
        return Ok(());
    };
    let runtime_client = hello.client_kind == "Runtime";
    let (mut reader, mut writer) = tokio::io::split(pipe);
    let (sender, mut receiver) = mpsc::channel::<Outbound>(MAX_QUEUED_FRAMES);
    let budget = Arc::new(Semaphore::new(MAX_QUEUED_BYTES));
    let mut writer_task = tokio::spawn(async move {
        while let Some(frame) = receiver.recv().await {
            writer.write_u32_le(frame.bytes.len() as u32).await?;
            writer.write_all(&frame.bytes).await?;
            writer.flush().await?;
        }
        Ok::<(), anyhow::Error>(())
    });
    let mut changes = handle.changes.clone();
    let mut subscriptions = HashMap::<String, Subscription>::new();
    let result: Result<()> = async {
        loop {
            // Keep consumed header/payload bytes when a service notification wins.
            let pending_frame = read_frame(&mut reader);
            tokio::pin!(pending_frame);
            loop {
            tokio::select! {
                frame = &mut pending_frame => {
                    let frame = match frame {
                        Ok(frame) => frame,
                        Err(error) => {
                            queue_control(&sender, &budget, &json!({"type":"protocol_error","code":"INVALID_FRAME","message":format!("{error:#}")})).await?;
                            return Err(error);
                        }
                    };
                    if frame["type"] != "request" {
                        queue_control(&sender, &budget, &json!({"type":"protocol_error","code":"INVALID_FRAME","message":"Expected a request frame"})).await?;
                        return Err(anyhow!("expected request frame"));
                    }
                    let request: Request = match serde_json::from_value(frame) {
                        Ok(request) => request,
                        Err(error) => {
                            queue_control(&sender, &budget, &json!({"type":"protocol_error","code":"INVALID_FRAME","message":error.to_string()})).await?;
                            return Err(error.into());
                        }
                    };
                    let response = if runtime_client {
                        failure(&request.request_id, "unsupported", "CAPABILITY_UNAVAILABLE",
                            "External runtime connections are not enabled; invocation authority is issued by the owned runtime")
                    } else if request.method == "events.subscribe" {
                        let params = &request.params;
                        let valid = params.as_object().is_some_and(|object| object.keys().all(|key| ["scope","afterCursor"].contains(&key.as_str())))
                            && request.command_id.is_none() && request.if_match.is_empty();
                        if !valid {
                            failure(&request.request_id,"error","INVALID_ARGUMENT","events.subscribe expects scope and optional afterCursor, without mutation fields")
                        } else {
                            let scope = params.get("scope").cloned().unwrap_or(Value::Null);
                            let valid_scope = scope.as_object().is_some_and(|object| object.keys().all(|key| ["kind","id"].contains(&key.as_str())))
                                && match scope["kind"].as_str() {
                                    Some("WorkList") => scope.get("id").is_none(),
                                    Some("Work" | "Conversation" | "Operation") => scope["id"].as_str().is_some_and(|id| !id.is_empty()),
                                    _ => false,
                                };
                            if !valid_scope || params.get("afterCursor").is_some_and(|v| !v.is_string()) {
                                failure(&request.request_id,"error","INVALID_ARGUMENT","Invalid subscription scope or cursor")
                            } else {
                                let subscription_id = Uuid::new_v4().to_string();
                                let snapshot = if let Some(after) = params["afterCursor"].as_str() {
                                    handle.events(after.to_owned(),scope.clone()).await.map(|_| (None,after.to_owned()))
                                } else {
                                    handle.snapshot(scope.clone()).await.map(|(view,cursor)| (Some(view),cursor))
                                };
                                match snapshot {
                                    Ok((snapshot,cursor)) => {
                                        let mut data = json!({"subscriptionId":subscription_id,"cursor":cursor});
                                        if let Some(snapshot) = snapshot { data["snapshot"] = snapshot; }
                                        subscriptions.insert(subscription_id,Subscription{scope,cursor:cursor.clone()});
                                        ok(&request.request_id,data,Some(&cursor))?
                                    }
                                    Err(mut error) => { error.request_id = request.request_id.clone(); error }
                                }
                            }
                        }
                    } else if request.method == "events.unsubscribe" {
                        let id = request.params["subscriptionId"].as_str().unwrap_or("");
                        if request.params.as_object().is_some_and(|o| o.len()==1) && request.command_id.is_none()
                            && request.if_match.is_empty() && subscriptions.remove(id).is_some() {
                            ok(&request.request_id,json!({"closed":true}),None)?
                        } else {
                            failure(&request.request_id,"error","INVALID_REFERENCE","Unknown subscription or invalid unsubscribe payload")
                        }
                    } else {
                        handle.request(Principal::Human,request).await
                    };
                    queue_control(&sender,&budget,&serde_json::to_value(response)?).await?;
                    send_events(&handle,&mut subscriptions,&sender,&budget).await?;
                    break;
                }
                changed = changes.changed() => {
                    changed.context("work event service stopped")?;
                    send_events(&handle,&mut subscriptions,&sender,&budget).await?;
                }
                result = &mut writer_task => { return result.context("outbound writer failed")?; }
            }
            }
        }
    }.await;
    drop(sender);
    // Bound disconnect cleanup; an unread pipe must never retain a service client forever.
    if !writer_task.is_finished() {
        if tokio::time::timeout(std::time::Duration::from_secs(2), &mut writer_task)
            .await
            .is_err()
        {
            writer_task.abort();
        }
    }
    result
}

async fn send_events(
    handle: &ServiceHandle,
    subscriptions: &mut HashMap<String, Subscription>,
    sender: &mpsc::Sender<Outbound>,
    budget: &Arc<Semaphore>,
) -> Result<()> {
    let mut closed = Vec::new();
    'subscriptions: for (id, subscription) in subscriptions.iter_mut() {
        match handle
            .events(subscription.cursor.clone(), subscription.scope.clone())
            .await
        {
            Ok((events, cursor)) => {
                for mut event in events {
                    event["type"] = json!("event");
                    event["subscriptionId"] = json!(id);
                    // Reserve bounded space for replies and the recovery marker.
                    // The cursor advances only after the entire batch was queued.
                    let bytes = serde_json::to_vec(&event)?.len();
                    if sender.capacity() <= RESERVED_CONTROL_FRAMES
                        || budget.available_permits() < bytes + MAX_FRAME_BYTES
                    {
                        let failure = Response::fail("", "RESYNC_REQUIRED",
                            "Service event consumer exceeded the bounded outbound budget; resubscribe with a snapshot").failure;
                        queue_control(
                            sender,
                            budget,
                            &json!({"type":"stream_error","subscriptionId":id,
                                "failure":failure,
                                "cursor":subscription.cursor}),
                        )
                        .await?;
                        tracing::warn!(target:"agent_center", subscription_id=%id, "RESYNC_REQUIRED: outbound event budget exhausted");
                        closed.push(id.clone());
                        continue 'subscriptions;
                    }
                    queue(sender, budget, &event)?;
                }
                subscription.cursor = cursor;
            }
            Err(response) => {
                let response = serde_json::to_value(response)?;
                queue_control(
                    sender,
                    budget,
                    &json!({"type":"stream_error","subscriptionId":id,"failure":response["failure"],"cursor":subscription.cursor}),
                ).await?;
                closed.push(id.clone());
            }
        }
    }
    for id in closed {
        subscriptions.remove(&id);
    }
    Ok(())
}

#[derive(Default)]
struct ReplyState {
    pending: HashMap<String, oneshot::Sender<Result<Response>>>,
    failure: Option<String>,
}

type PendingReplies = Arc<Mutex<ReplyState>>;

struct PendingRequest {
    replies: PendingReplies,
    id: String,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        if let Ok(mut replies) = self.replies.lock() {
            replies.pending.remove(&self.id);
        }
    }
}

#[derive(Clone)]
enum EventStatus {
    Live,
    ResyncRequired(String),
    Closed(String),
}

fn publish_resync_required(status: &watch::Sender<EventStatus>, message: String) {
    // abort() cannot interrupt a reader poll already running on another thread.
    // Never let its delayed overflow notification replace a terminal failure.
    status.send_if_modified(|current| {
        if matches!(current, EventStatus::Live) {
            *current = EventStatus::ResyncRequired(message);
            true
        } else {
            false
        }
    });
}

fn publish_connection_failure(
    replies: &PendingReplies,
    status: &watch::Sender<EventStatus>,
    message: String,
) {
    let message = if let Ok(mut replies) = replies.lock() {
        let message = replies.failure.get_or_insert(message).clone();
        for (_, reply) in replies.pending.drain() {
            let _ = reply.send(Err(anyhow!(message.clone())));
        }
        message
    } else {
        message
    };
    status.send_if_modified(|current| {
        if matches!(current, EventStatus::Closed(_)) {
            false
        } else {
            *current = EventStatus::Closed(message);
            true
        }
    });
}

struct InboundEvent {
    value: Value,
    _permit: OwnedSemaphorePermit,
}

pub(crate) struct Client {
    writer:
        tokio::sync::Mutex<tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>>,
    pending: PendingReplies,
    events: tokio::sync::Mutex<mpsc::Receiver<InboundEvent>>,
    event_status: watch::Receiver<EventStatus>,
    status_sender: watch::Sender<EventStatus>,
    pipe_name: String,
    store_id: String,
    reader_task: tokio::task::JoinHandle<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

struct RequestWrite<'a> {
    client: &'a Client,
    complete: bool,
}

impl Drop for RequestWrite<'_> {
    fn drop(&mut self) {
        if !self.complete {
            // A cancelled length/payload write cannot safely share this stream
            // with the next request, even if the caller no longer wants a reply.
            self.client
                .close("WRITE_CANCELLED: request frame may be incomplete; reconnect and reconcile");
        }
    }
}

impl Client {
    pub(crate) async fn connect() -> Result<Self> {
        let root = state_root()?;
        tokio::fs::create_dir_all(&root)
            .await
            .context("creating Agent Center state directory")?;
        let name = pipe_name(&root)?;
        match ClientOptions::new().open(&name) {
            Ok(pipe) => return Self::from_pipe(pipe, name).await,
            Err(error) if matches!(error.raw_os_error(), Some(2 | 231)) => {}
            Err(error) => return Err(error).context("connecting to private Agent Center service"),
        }
        let mut child = Self::start_service_process()?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            match ClientOptions::new().open(&name) {
                Ok(pipe) => return Self::from_pipe(pipe, name).await,
                Err(error) if matches!(error.raw_os_error(), Some(2 | 231)) => {}
                Err(error) => return Err(error).context("connecting to new Agent Center service"),
            }
            if tokio::time::Instant::now() >= deadline {
                let status = child
                    .try_wait()
                    .context("checking Agent Center service startup")?;
                bail!("Agent Center did not become available within 15 seconds (process status: {status:?}); inspect wta-center-service logs or run `wta center serve`");
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }

    #[cfg(test)]
    async fn connect_to(name: &str) -> Result<Self> {
        let pipe = ClientOptions::new()
            .open(name)
            .context("Agent Center is unavailable; run `wta center serve` in another terminal")?;
        Self::from_pipe(pipe, name.to_owned()).await
    }

    pub(crate) async fn reconnect(&self) -> Result<Self> {
        // Reconnect only to this authority. Never spawn a replacement authority or
        // replay a request whose effect might already have been committed.
        let pipe = ClientOptions::new()
            .open(&self.pipe_name)
            .context("reconnecting to the same Agent Center authority")?;
        let client = Self::from_pipe(pipe, self.pipe_name.clone()).await?;
        if client.store_id != self.store_id {
            bail!("STORE_CHANGED: Agent Center authority changed; automatic recovery refused");
        }
        Ok(client)
    }

    async fn from_pipe(
        pipe: tokio::net::windows::named_pipe::NamedPipeClient,
        name: String,
    ) -> Result<Self> {
        tokio::time::timeout(REQUEST_TIMEOUT, Self::negotiate_pipe(pipe, name))
            .await
            .context("Agent Center handshake timed out")?
    }

    async fn negotiate_pipe(
        mut pipe: tokio::net::windows::named_pipe::NamedPipeClient,
        name: String,
    ) -> Result<Self> {
        write_frame(&mut pipe,&json!({"type":"hello","versions":[1],"clientInstanceId":Uuid::new_v4().to_string(),"clientKind":"Console"})).await?;
        let welcome = read_frame(&mut pipe).await?;
        if welcome["type"] != "welcome"
            || welcome["version"] != 1
            || welcome["maxFrameBytes"] != MAX_FRAME_BYTES
            || !welcome["storeId"].is_string()
        {
            bail!("Agent Center did not negotiate protocol v1: {welcome}");
        }
        let (mut reader, writer) = tokio::io::split(pipe);
        let pending = PendingReplies::default();
        let replies = pending.clone();
        let (event_sender, events) = mpsc::channel(MAX_QUEUED_FRAMES);
        let event_budget = Arc::new(Semaphore::new(MAX_QUEUED_BYTES));
        let (status_sender, event_status) = watch::channel(EventStatus::Live);
        let reader_status = status_sender.clone();
        let reader_task = tokio::spawn(async move {
            let mut resync_required = false;
            let failure = loop {
                let frame = match read_frame(&mut reader).await {
                    Ok(frame) => frame,
                    Err(error) => break error,
                };
                match frame["type"].as_str() {
                    Some("response") => {
                        let response = match serde_json::from_value::<Response>(frame) {
                            Ok(response) => response,
                            Err(error) => break error.into(),
                        };
                        let reply = match replies.lock() {
                            Ok(mut replies) => replies.pending.remove(&response.request_id),
                            Err(_) => break anyhow!("response routing lock poisoned"),
                        };
                        if let Some(reply) = reply {
                            let _ = reply.send(Ok(response));
                        } else {
                            // A cancelled caller no longer needs its reply.
                            tracing::debug!(target:"agent_center", request_id=%response.request_id, "Discarding reply for a cancelled request");
                        }
                    }
                    Some("event" | "stream_error") => {
                        if resync_required {
                            continue;
                        }
                        let bytes = match serde_json::to_vec(&frame) {
                            Ok(bytes) => bytes.len(),
                            Err(error) => break error.into(),
                        };
                        let queued = match event_budget.clone().try_acquire_many_owned(bytes as u32)
                        {
                            Ok(permit) => event_sender
                                .try_send(InboundEvent {
                                    value: frame,
                                    _permit: permit,
                                })
                                .map_err(|_| "frame queue"),
                            Err(_) => Err("byte budget"),
                        };
                        if let Err(limit) = queued {
                            // Stop delivering this generation, not reading the pipe.
                            // The out-of-band marker cannot be hidden by a full queue.
                            resync_required = true;
                            let message = format!("RESYNC_REQUIRED: client event {limit} exhausted (maximum {MAX_QUEUED_FRAMES} frames, {MAX_QUEUED_BYTES} bytes); reconnect and resubscribe with snapshots");
                            tracing::warn!(target:"agent_center", cause=%message, "Client event consumer fell behind");
                            publish_resync_required(&reader_status, message);
                        }
                    }
                    Some("protocol_error") => {
                        break anyhow!(
                            "Agent Center protocol error {}: {}",
                            frame["code"].as_str().unwrap_or("UNKNOWN"),
                            frame["message"]
                                .as_str()
                                .unwrap_or("No diagnostic supplied")
                        )
                    }
                    _ => break anyhow!("unexpected Agent Center frame type"),
                }
            };
            let message = format!("Agent Center transport closed: {failure:#}");
            tracing::warn!(target:"agent_center", cause=%message, "Agent Center connection reader stopped");
            publish_connection_failure(&replies, &reader_status, message);
        });
        Ok(Self {
            writer: tokio::sync::Mutex::new(writer),
            pending,
            events: tokio::sync::Mutex::new(events),
            event_status,
            status_sender,
            pipe_name: name,
            store_id: welcome["storeId"].as_str().unwrap_or_default().to_owned(),
            reader_task,
        })
    }

    fn start_service_process() -> Result<std::process::Child> {
        use std::os::windows::process::CommandExt;
        use std::process::{Command, Stdio};
        use windows_sys::Win32::System::Threading::{
            CREATE_BREAKAWAY_FROM_JOB, CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS,
        };
        // A Console is disposable presentation. Refuse startup if its job cannot
        // release the authority instead of silently tying work to window closure.
        Command::new(std::env::current_exe().context("locating the WTA executable")?)
            .args(["center", "serve"])
            .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB | CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS)
            .spawn()
            .context("cannot start the independent Agent Center service; run `wta center serve` outside the Console's process job")
    }

    pub(crate) async fn request(&self, request: Request) -> Result<Response> {
        let identity = if let Some(command) = request.command_id.as_deref() {
            format!("OUTCOME_UNKNOWN: requestId={} commandId={command}; reconcile the recorded command before any retry", request.request_id)
        } else {
            format!("Agent Center read requestId={}", request.request_id)
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, self.request_inner(request)).await {
            Ok(result) => result.with_context(|| identity),
            Err(_) => {
                let message = "REQUEST_TIMEOUT: Agent Center did not respond within 10 seconds; reconnect and reconcile";
                self.close(message);
                Err(anyhow!(message)).with_context(|| identity)
            }
        }
    }

    fn close(&self, message: &str) {
        tracing::warn!(target:"agent_center", cause=message, "Closing Agent Center client transport");
        publish_connection_failure(&self.pending, &self.status_sender, message.to_owned());
        self.reader_task.abort();
    }

    async fn request_inner(&self, request: Request) -> Result<Response> {
        let value = serde_json::to_value(&request)?;
        let id = request.request_id.clone();
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self
                .pending
                .lock()
                .map_err(|_| anyhow!("request routing lock poisoned"))?;
            if let Some(failure) = &pending.failure {
                bail!("{failure}");
            }
            if pending.pending.contains_key(&id) {
                bail!("requestId is already outstanding");
            }
            pending.pending.insert(id.clone(), sender);
        }
        let _pending = PendingRequest {
            replies: self.pending.clone(),
            id,
        };
        let result = {
            let mut writer = self.writer.lock().await;
            if let Some(failure) = &self
                .pending
                .lock()
                .map_err(|_| anyhow!("request routing lock poisoned"))?
                .failure
            {
                bail!("{failure}");
            }
            let mut write = RequestWrite {
                client: self,
                complete: false,
            };
            let result = write_frame(&mut *writer, &value).await;
            write.complete = true;
            result
        };
        if let Err(error) = result {
            self.close(&format!("Agent Center request write failed: {error:#}"));
            return Err(error);
        }
        receiver
            .await
            .context("Agent Center disconnected before responding")?
    }

    pub(crate) async fn next_event(&self) -> Result<Value> {
        let mut events = self.events.lock().await;
        let mut status = self.event_status.clone();
        loop {
            match status.borrow_and_update().clone() {
                EventStatus::Live => {}
                EventStatus::ResyncRequired(message) => bail!("{message}"),
                EventStatus::Closed(message) => bail!("{message}"),
            }
            tokio::select! {
                biased;
                changed = status.changed() => {
                    changed.context("Agent Center event status closed")?;
                }
                event = events.recv() => {
                    return event.map(|event| event.value)
                        .context("Agent Center event stream closed without a diagnostic");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn explicit_agent_center_state_is_absolute_and_does_not_change_the_default() {
        let application = PathBuf::from(r"C:\LocalState\IntelligentTerminal");
        assert_eq!(
            super::resolve_state_root(None, Some(application.clone())).unwrap(),
            application.join("agent-center")
        );
        let isolated = PathBuf::from(r"C:\Experiments\real-work");
        assert_eq!(
            super::resolve_state_root(Some(isolated.clone()), None).unwrap(),
            isolated
        );
        for value in ["", "relative", r"C:relative", r"\root-relative"] {
            assert!(super::resolve_state_root(
                Some(PathBuf::from(value)),
                Some(application.clone())
            )
            .is_err());
        }
        assert!(super::resolve_state_root(None, None).is_err());
    }

    use super::*;
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context as TaskContext, Poll};
    use tokio::io::ReadBuf;

    struct ObservedPipe {
        pipe: NamedPipeServer,
        consumed: watch::Sender<usize>,
    }

    impl AsyncRead for ObservedPipe {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let before = buffer.filled().len();
            let result = Pin::new(&mut self.pipe).poll_read(cx, buffer);
            let count = buffer.filled().len() - before;
            if count != 0 {
                self.consumed.send_modify(|total| *total += count);
            }
            result
        }
    }

    impl AsyncWrite for ObservedPipe {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Pin::new(&mut self.pipe).poll_write(cx, bytes)
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.pipe).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.pipe).poll_shutdown(cx)
        }
    }

    struct GatedWriter {
        pipe: NamedPipeServer,
        release: oneshot::Receiver<()>,
        negotiated: bool,
        released: bool,
    }

    impl AsyncRead for GatedWriter {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.pipe).poll_read(cx, buffer)
        }
    }

    impl AsyncWrite for GatedWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
            bytes: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.negotiated && !self.released {
                if Pin::new(&mut self.release).poll(cx).is_pending() {
                    return Poll::Pending;
                }
                self.released = true;
            }
            Pin::new(&mut self.pipe).poll_write(cx, bytes)
        }

        fn poll_flush(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            let result = Pin::new(&mut self.pipe).poll_flush(cx);
            if result.is_ready() {
                self.negotiated = true;
            }
            result
        }

        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut TaskContext<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.pipe).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn named_pipe_service_event_overflow_reserves_replies_and_resync_marker() {
        let (sender, mut requests) = mpsc::channel(16);
        let (_changes, changes) = watch::channel(0);
        let (processed, processing) = oneshot::channel();
        let actor = tokio::spawn(async move {
            let mut processed = Some(processed);
            let mut event_reads = 0;
            while let Some(message) = requests.recv().await {
                match message {
                    EngineMessage::Snapshot(_, reply) => {
                        reply
                            .send(Ok((json!({"items":[]}), "store:0".to_owned())))
                            .unwrap();
                    }
                    EngineMessage::Events(_, _, reply) => {
                        event_reads += 1;
                        assert_eq!(event_reads, 1, "overflowed subscription must be removed");
                        reply.send(Ok(((1..=MAX_QUEUED_FRAMES + 32)
                                .map(|sequence| json!({"cursor":format!("store:{sequence}"),"kind":"WorkUpdated"}))
                                .collect(), "store:288".to_owned()))).unwrap();
                    }
                    EngineMessage::Request(_, request, reply) => {
                        reply
                            .send(Response::ok(request.request_id, json!({"items":[]})))
                            .unwrap();
                        if let Some(processed) = processed.take() {
                            processed.send(()).unwrap();
                        }
                    }
                    _ => panic!("unexpected fixture request"),
                }
            }
        });
        let handle = ServiceHandle {
            sender,
            changes,
            store_id: "store".to_owned(),
        };
        let name = format!(
            r"\\.\pipe\AgentCenter-server-backpressure-{}",
            Uuid::new_v4()
        );
        let server = create_pipe(&name, true).unwrap();
        let (release, released) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            connection(
                GatedWriter {
                    pipe: server,
                    release: released,
                    negotiated: false,
                    released: false,
                },
                handle,
                "instance".to_owned(),
            )
            .await
        });
        let mut client = ClientOptions::new().open(&name).unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            write_frame(
                &mut client,
                &json!({"type":"hello","versions":[1],
                    "clientInstanceId":Uuid::new_v4().to_string(),"clientKind":"Console"}),
            )
            .await?;
            anyhow::ensure!(read_frame(&mut client).await?["type"] == "welcome");
            let subscribe = Request::new("events.subscribe", json!({"scope":{"kind":"WorkList"}}));
            let read = Request::new("work.list", json!({"limit":10}));
            write_frame(&mut client, &serde_json::to_value(&subscribe)?).await?;
            write_frame(&mut client, &serde_json::to_value(&read)?).await?;
            processing.await?;
            release.send(()).unwrap();
            let mut saw_resync = false;
            let mut responses = Vec::new();
            loop {
                let frame = read_frame(&mut client).await?;
                match frame["type"].as_str() {
                    Some("response") => responses.push(frame["requestId"].clone()),
                    Some("stream_error") => {
                        anyhow::ensure!(frame["failure"]["code"] == "RESYNC_REQUIRED");
                        saw_resync = true;
                    }
                    Some("event") => {
                        anyhow::ensure!(!saw_resync, "event arrived after its subscription closed")
                    }
                    _ => bail!("unexpected frame: {frame}"),
                }
                if responses.len() == 2 {
                    break;
                }
            }
            anyhow::ensure!(saw_resync);
            anyhow::ensure!(responses == vec![json!(subscribe.request_id), json!(read.request_id)]);
            let read = Request::new("project.list", json!({"limit":10}));
            write_frame(&mut client, &serde_json::to_value(&read)?).await?;
            anyhow::ensure!(read_frame(&mut client).await?["requestId"] == read.request_id);
            Ok::<(), anyhow::Error>(())
        })
        .await;
        drop(client);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .unwrap()
            .unwrap();
        actor.await.unwrap();
        result.unwrap().unwrap();
    }

    #[tokio::test]
    async fn fragmented_requests_survive_service_changes_and_keep_delivering_events() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("wta-center-fragments-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let handle = start_engine(root.clone()).await.unwrap();
        let runtime_id = Uuid::new_v4().to_string();
        let mut registration = Request::new(
            "runtime.register",
            json!({
                "runtimeInstanceId":runtime_id,"protocolVersions":[1],"capabilities":[
                    {"id":"fragment-fixture","kinds":["ProduceResult","Coordinate"],"supportsContinuation":true,"supportsScopedStop":true},
                    {"id":"native-check","kinds":["EvaluateGate"],"supportsContinuation":false,"supportsScopedStop":true}
                ]
            }),
        );
        registration.command_id = Some(Uuid::new_v4().to_string());
        assert_eq!(
            handle
                .request(Principal::Runtime { runtime_id }, registration)
                .await
                .status,
            "ok"
        );
        let mut configure = Request::new(
            "project.configure",
            json!({
                "name":"Fragmented pipe fixture","root":root,
                "coordinatorCapabilityId":"fragment-fixture","workerCapabilityId":"fragment-fixture","checkCapabilityId":"native-check",
                "limits":{"concurrency":1,"executionAttempts":1,"evaluationAttempts":1,"coordinationTurns":1,
                    "contextRounds":1,"executionSeconds":30,"coordinationSeconds":30}
            }),
        );
        configure.command_id = Some(Uuid::new_v4().to_string());
        let project = handle.request(Principal::Human, configure).await;
        assert_eq!(project.status, "ok", "{project:?}");
        let project_id = project.data.unwrap()["projectId"].clone();

        let name = format!(r"\\.\pipe\AgentCenter-fragments-{}", Uuid::new_v4());
        let server = create_pipe(&name, true).unwrap();
        let mut client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let (consumed, mut observed) = watch::channel(0);
        let service = handle.clone();
        let server_task = tokio::spawn(connection(
            ObservedPipe {
                pipe: server,
                consumed,
            },
            service,
            Uuid::new_v4().to_string(),
        ));
        let trigger_name = format!(r"\\.\pipe\AgentCenter-trigger-{}", Uuid::new_v4());
        let trigger_server = create_pipe(&trigger_name, true).unwrap();
        let service = handle.clone();
        let trigger_task = tokio::spawn(async move {
            trigger_server.connect().await?;
            connection(trigger_server, service, Uuid::new_v4().to_string()).await
        });
        let trigger = Client::connect_to(&trigger_name).await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            write_frame(&mut client, &json!({
                "type":"hello","versions":[1],"clientInstanceId":Uuid::new_v4().to_string(),"clientKind":"Console"
            })).await?;
            anyhow::ensure!(read_frame(&mut client).await?["type"] == "welcome");
            let subscribe = Request::new("events.subscribe", json!({"scope":{"kind":"WorkList"}}));
            write_frame(&mut client, &serde_json::to_value(&subscribe)?).await?;
            let subscribed = read_frame(&mut client).await?;
            anyhow::ensure!(subscribed["requestId"] == subscribe.request_id && subscribed["status"] == "ok");
            let subscription = subscribed["data"]["subscriptionId"].clone();

            // Consumption and event receipts, not sleeps, establish the race ordering.
            for fragmented in [false, true] {
                let request = Request::new("work.list", json!({"limit":10}));
                let payload = serde_json::to_vec(&request)?;
                let mut packet = (payload.len() as u32).to_le_bytes().to_vec();
                packet.extend_from_slice(&payload);
                let base = *observed.borrow();
                let boundaries = if fragmented {
                    vec![1, 2, 3, 4, 5, 11, packet.len() - 1]
                } else {
                    vec![4]
                };
                let mut sent = 0;
                for end in boundaries {
                    client.write_all(&packet[sent..end]).await?;
                    client.flush().await?;
                    observed.wait_for(|total| *total >= base + end).await?;
                    let mut draft = Request::new("work.create_draft", json!({
                        "projectId":project_id,"goal":"Notification without model execution",
                        "scope":["report.txt"],"exclusions":[],"context":[],"sourceMessageIds":[],
                        "criteria":[{"id":"report","description":"A report exists","evidenceRule":"artifact:report"}],
                        "delivery":{"kind":"Report"}
                    }));
                    draft.command_id = Some(Uuid::new_v4().to_string());
                    let response = trigger.request(draft).await?;
                    anyhow::ensure!(response.status == "ok", "change trigger failed: {response:?}");
                    loop {
                        let event = read_frame(&mut client).await?;
                        anyhow::ensure!(event["type"] == "event" && event["subscriptionId"] == subscription,
                            "service event did not arrive during a partial frame: {event}");
                        if event["cursor"].as_str() == response.cursor.as_deref() {
                            break;
                        }
                    }
                    sent = end;
                }
                client.write_all(&packet[sent..]).await?;
                client.flush().await?;
                let response = read_frame(&mut client).await?;
                anyhow::ensure!(response["type"] == "response"
                    && response["requestId"] == request.request_id && response["status"] == "ok",
                    "valid fragmented request lost framing or correlation: {response}");
            }
            let request = Request::new("project.list", json!({"limit":10}));
            write_frame(&mut client, &serde_json::to_value(&request)?).await?;
            let response = read_frame(&mut client).await?;
            anyhow::ensure!(response["requestId"] == request.request_id && response["status"] == "ok");
            let base = *observed.borrow();
            client.write_all(&[32, 0]).await?;
            observed.wait_for(|total| *total >= base + 2).await?;
            Ok::<(), anyhow::Error>(())
        }).await.context("fragmented pipe did not make bounded progress").and_then(|result| result);
        drop(client);
        drop(trigger);
        for task in [server_task, trigger_task] {
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(5), task)
                    .await
                    .expect("partial disconnect must release the pipe handler")
                    .unwrap()
                    .is_err()
            );
        }
        handle.shutdown().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
        result.unwrap();
    }

    #[tokio::test]
    async fn partial_payload_disconnect_and_service_shutdown_release_the_reader() {
        for stop_service in [false, true] {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!("wta-center-disconnect-{}", Uuid::new_v4()));
            tokio::fs::create_dir_all(&root).await.unwrap();
            let handle = start_engine(root.clone()).await.unwrap();
            let name = format!(r"\\.\pipe\AgentCenter-partial-close-{}", Uuid::new_v4());
            let server = create_pipe(&name, true).unwrap();
            let mut client = ClientOptions::new().open(&name).unwrap();
            server.connect().await.unwrap();
            let (consumed, mut observed) = watch::channel(0);
            let task = tokio::spawn(connection(
                ObservedPipe {
                    pipe: server,
                    consumed,
                },
                handle.clone(),
                Uuid::new_v4().to_string(),
            ));
            write_frame(&mut client, &json!({
                "type":"hello","versions":[1],"clientInstanceId":Uuid::new_v4().to_string(),"clientKind":"CLI"
            })).await.unwrap();
            assert_eq!(read_frame(&mut client).await.unwrap()["type"], "welcome");
            let base = *observed.borrow();
            client.write_all(&[32, 0, 0, 0, b'{']).await.unwrap();
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                observed.wait_for(|total| *total >= base + 5),
            )
            .await
            .unwrap()
            .unwrap();
            if stop_service {
                handle.shutdown().await.unwrap();
            } else {
                drop(client);
            }
            assert!(
                tokio::time::timeout(std::time::Duration::from_secs(5), task)
                    .await
                    .expect("incomplete payload must not keep a dead connection alive")
                    .unwrap()
                    .is_err()
            );
            if !stop_service {
                handle.shutdown().await.unwrap();
            }
            tokio::fs::remove_dir_all(root).await.unwrap();
        }
    }

    #[tokio::test]
    async fn private_pipe_routes_requests_to_the_durable_actor() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("wta-center-pipe-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let handle = start_engine(root.clone()).await.unwrap();
        let name = format!(r"\\.\pipe\AgentCenter-actor-test-{}", Uuid::new_v4());
        let server = create_pipe(&name, true).unwrap();
        let service = handle.clone();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            connection(server, service, Uuid::new_v4().to_string()).await
        });
        let client = Client::connect_to(&name).await.unwrap();
        let request = Request::new("project.list", json!({"limit": 10}));
        let request_id = request.request_id.clone();
        let response = client.request(request).await.unwrap();
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.status, "ok");
        assert_eq!(response.data.unwrap()["items"], json!([]));

        let rejected = client
            .request(Request::new("work.create_draft", json!({})))
            .await
            .unwrap();
        assert_eq!(rejected.status, "error");
        assert!(rejected.failure.is_some());
        assert_eq!(
            client
                .request(Request::new("work.list", json!({"limit": 10})))
                .await
                .unwrap()
                .status,
            "ok"
        );

        drop(client);
        let disconnected = tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .expect("disconnected Console must release its pipe handler")
            .unwrap();
        assert!(disconnected.is_err());
        handle.shutdown().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn adapter_configuration_is_validated_locked_and_atomically_replaced() {
        let directory = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("wta-center-config-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&directory).await.unwrap();
        let input = directory.join("input.json");
        let root = directory.join("state");
        let first = br#"{"capabilities":[]}"#;
        tokio::fs::write(&input, first).await.unwrap();
        configure_at(&input, &root).await.unwrap();
        let installed = root.join("adapters.json");
        assert_eq!(tokio::fs::read(&installed).await.unwrap(), first);

        tokio::fs::write(&input, b"not JSON").await.unwrap();
        assert!(configure_at(&input, &root).await.is_err());
        assert_eq!(tokio::fs::read(&installed).await.unwrap(), first);

        let replacement = b"{ \"capabilities\": [] }\n";
        tokio::fs::write(&input, replacement).await.unwrap();
        let authority = lock_authority(&root).unwrap();
        assert!(configure_at(&input, &root).await.is_err());
        assert_eq!(tokio::fs::read(&installed).await.unwrap(), first);
        drop(authority);
        configure_at(&input, &root).await.unwrap();
        assert_eq!(tokio::fs::read(&installed).await.unwrap(), replacement);

        tokio::fs::write(&input, vec![b' '; MAX_FRAME_BYTES + 1])
            .await
            .unwrap();
        assert!(configure_at(&input, &root)
            .await
            .unwrap_err()
            .to_string()
            .contains("exceeds 1 MiB"));
        assert_eq!(tokio::fs::read(&installed).await.unwrap(), replacement);
        assert!(!std::fs::read_dir(&root).unwrap().any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|extension| extension == "staging")));
        tokio::fs::remove_dir_all(directory).await.unwrap();
    }

    #[tokio::test]
    async fn frames_round_trip_unicode_and_reject_nonobjects() {
        let (mut writer, mut reader) = tokio::io::duplex(1024);
        let value = json!({"message":"\u{4ea4}\u{4ed8}","id":Uuid::new_v4().to_string()});
        write_frame(&mut writer, &value).await.unwrap();
        assert_eq!(read_frame(&mut reader).await.unwrap(), value);
        for invalid in [b"[]".as_slice(), b"{".as_slice(), b"\xff".as_slice()] {
            writer.write_u32_le(invalid.len() as u32).await.unwrap();
            writer.write_all(invalid).await.unwrap();
            assert!(read_frame(&mut reader)
                .await
                .unwrap_err()
                .to_string()
                .contains("INVALID_FRAME"));
        }
    }

    #[tokio::test]
    async fn oversized_frame_is_rejected_before_allocating_body() {
        let (mut writer, mut reader) = tokio::io::duplex(32);
        for length in [0, MAX_FRAME_BYTES as u32 + 1, u32::MAX] {
            writer.write_u32_le(length).await.unwrap();
            assert!(read_frame(&mut reader)
                .await
                .unwrap_err()
                .to_string()
                .contains("INVALID_FRAME"));
        }
    }

    #[test]
    fn outbound_limits_are_enforced_and_released() {
        let (sender, mut receiver) = mpsc::channel(1);
        let budget = Arc::new(Semaphore::new(32));
        queue(&sender, &budget, &json!({"a":1})).unwrap();
        assert!(queue(&sender, &budget, &json!({"a":2})).is_err());
        drop(receiver.try_recv().unwrap());
        assert_eq!(budget.available_permits(), 32);
        queue(&sender, &budget, &json!({"a":3})).unwrap();
        let (sender, _receiver) = mpsc::channel(5);
        let budget = Arc::new(Semaphore::new(1));
        assert!(queue(&sender, &budget, &json!({"a":1})).is_err());
    }

    #[tokio::test]
    async fn incompatible_hello_is_rejected_without_opening_work_store() {
        let name = format!(r"\\.\pipe\AgentCenter-hello-test-{}", Uuid::new_v4());
        let server = create_pipe(&name, true).unwrap();
        let mut client = ClientOptions::new().open(&name).unwrap();
        server.connect().await.unwrap();
        let task = tokio::spawn(async move {
            let mut server = server;
            assert!(negotiate(&mut server, "test-store", "test-instance")
                .await
                .unwrap()
                .is_none());
        });
        write_frame(&mut client,&json!({
            "type":"hello","versions":[2],"clientInstanceId":Uuid::new_v4().to_string(),"clientKind":"CLI"
        })).await.unwrap();
        let result = read_frame(&mut client).await.unwrap();
        assert_eq!(result["code"], "INCOMPATIBLE_VERSION");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn client_correlates_reversed_responses_and_interleaved_events() {
        let name = format!(r"\\.\pipe\AgentCenter-client-test-{}", Uuid::new_v4());
        let mut server = create_pipe(&name, true).unwrap();
        let (release, released) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            negotiate(&mut server, "test-store", "test-instance")
                .await
                .unwrap();
            let first = read_frame(&mut server).await.unwrap();
            let second = read_frame(&mut server).await.unwrap();
            write_frame(
                &mut server,
                &json!({"type":"event","subscriptionId":"test-subscription","cursor":"one"}),
            )
            .await
            .unwrap();
            for request in [second, first] {
                write_frame(
                    &mut server,
                    &json!({
                        "type":"response","requestId":request["requestId"],"status":"ok",
                        "data":{"method":request["method"]},"subjects":[]
                    }),
                )
                .await
                .unwrap();
            }
            let _ = released.await;
        });
        let client = Client::connect_to(&name).await.unwrap();
        let request = |method| {
            serde_json::from_value(json!({
                "type":"request","requestId":Uuid::new_v4().to_string(),
                "method":method,"ifMatch":[],"params":{}
            }))
            .unwrap()
        };
        let (first, second) = tokio::join!(
            client.request(request("work.list")),
            client.request(request("project.list"))
        );
        assert_eq!(
            serde_json::to_value(first.unwrap()).unwrap()["data"]["method"],
            "work.list"
        );
        assert_eq!(
            serde_json::to_value(second.unwrap()).unwrap()["data"]["method"],
            "project.list"
        );
        assert_eq!(client.next_event().await.unwrap()["cursor"], "one");
        release.send(()).unwrap();
        server_task.await.unwrap();
    }

    async fn loaded_client_boundary(event_count: usize, payload_bytes: usize, overflow: bool) {
        let name = format!(r"\\.\pipe\AgentCenter-loaded-{}", Uuid::new_v4());
        let mut server = create_pipe(&name, true).unwrap();
        let (release, released) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            negotiate(&mut server, "loaded-store", "loaded-instance")
                .await
                .unwrap();
            let first = read_frame(&mut server).await.unwrap();
            let second = read_frame(&mut server).await.unwrap();
            for sequence in 0..event_count {
                write_frame(
                    &mut server,
                    &json!({
                        "type":"event","subscriptionId":"loaded-subscription",
                        "cursor":sequence.to_string(),"payload":"x".repeat(payload_bytes)
                    }),
                )
                .await
                .unwrap();
            }
            for request in [second, first] {
                write_frame(&mut server, &json!({
                    "type":"response","requestId":request["requestId"],"status":"ok","subjects":[],
                    "data":{"method":request["method"],"commandId":request["commandId"]}
                })).await.unwrap();
            }
            let _ = released.await;
        });
        let client = Client::connect_to(&name).await.unwrap();
        let command_id = Uuid::new_v4().to_string();
        let mut mutation = Request::new("work.update", json!({}));
        mutation.command_id = Some(command_id.clone());
        let (read, mutation) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                client.request(Request::new("work.list", json!({"limit":10}))),
                client.request(mutation)
            )
        })
        .await
        .expect("event load must not strand correlated replies");
        assert_eq!(read.unwrap().data.unwrap()["method"], "work.list");
        assert_eq!(mutation.unwrap().data.unwrap()["commandId"], command_id);
        if overflow {
            let error = client.next_event().await.unwrap_err().to_string();
            assert!(error.contains("RESYNC_REQUIRED"), "{error}");
        }
        // A physical EOF must replace, not be hidden by, a full event queue or
        // the earlier overflow marker.
        release.send(()).unwrap();
        server_task.await.unwrap();
        let mut status = client.event_status.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            status.wait_for(|status| matches!(status, EventStatus::Closed(_))),
        )
        .await
        .unwrap()
        .unwrap();
        let error = client.next_event().await.unwrap_err().to_string();
        assert!(
            error.contains("transport closed") && error.contains("reading frame length"),
            "{error}"
        );
        let error = client
            .request(Request::new("work.list", json!({})))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("reading frame length"));
    }

    #[tokio::test]
    async fn named_pipe_event_overflow_preserves_concurrent_read_and_mutation_receipts() {
        loaded_client_boundary(MAX_QUEUED_FRAMES + 32, 0, true).await;
    }

    #[tokio::test]
    async fn named_pipe_event_byte_budget_has_explicit_recovery() {
        loaded_client_boundary(12, 800_000, true).await;
    }

    #[tokio::test]
    async fn named_pipe_physical_loss_cause_survives_a_full_event_queue() {
        loaded_client_boundary(MAX_QUEUED_FRAMES, 0, false).await;
    }

    #[tokio::test]
    async fn named_pipe_terminal_close_survives_late_overflow_publication() {
        let name = format!(r"\\.\pipe\AgentCenter-terminal-status-{}", Uuid::new_v4());
        let mut server = create_pipe(&name, true).unwrap();
        let (release, released) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            negotiate(&mut server, "store", "instance").await.unwrap();
            released.await.unwrap();
        });
        let client = Client::connect_to(&name).await.unwrap();
        let delayed_reader = client.status_sender.clone();
        let original = "WRITE_CANCELLED: original incomplete request frame";
        client.close(original);
        // Model an overflow already detected by a concurrent reader poll: it
        // publishes after close(), despite that reader's abort being requested.
        publish_resync_required(&delayed_reader, "RESYNC_REQUIRED: late overflow".to_owned());
        assert_eq!(client.next_event().await.unwrap_err().to_string(), original);
        publish_connection_failure(&client.pending, &delayed_reader, "later EOF".to_owned());
        assert_eq!(client.next_event().await.unwrap_err().to_string(), original);
        let error = client
            .request(Request::new("work.list", json!({})))
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains(original));
        assert_eq!(
            client.pending.lock().unwrap().failure.as_deref(),
            Some(original)
        );
        release.send(()).unwrap();
        server_task.await.unwrap();
    }

    #[test]
    fn event_status_allows_only_live_to_resync_and_resync_to_closed() {
        let replies = PendingReplies::default();
        let (sender, receiver) = watch::channel(EventStatus::Live);
        publish_resync_required(&sender, "first overflow".to_owned());
        publish_resync_required(&sender, "later overflow".to_owned());
        assert!(matches!(&*receiver.borrow(),
            EventStatus::ResyncRequired(message) if message == "first overflow"));
        publish_connection_failure(&replies, &sender, "physical EOF".to_owned());
        publish_resync_required(&sender, "late overflow".to_owned());
        assert!(matches!(&*receiver.borrow(),
            EventStatus::Closed(message) if message == "physical EOF"));
    }

    #[tokio::test]
    async fn named_pipe_lost_mutation_receipt_reports_original_command_identity() {
        let name = format!(r"\\.\pipe\AgentCenter-indeterminate-{}", Uuid::new_v4());
        let mut server = create_pipe(&name, true).unwrap();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            negotiate(&mut server, "store", "instance").await.unwrap();
            let request = read_frame(&mut server).await.unwrap();
            for sequence in 0..MAX_QUEUED_FRAMES {
                write_frame(
                    &mut server,
                    &json!({"type":"event",
                    "subscriptionId":"old","cursor":sequence.to_string()}),
                )
                .await
                .unwrap();
            }
            request
        });
        let client = Client::connect_to(&name).await.unwrap();
        let mut request = Request::new("work.update", json!({}));
        let command_id = Uuid::new_v4().to_string();
        request.command_id = Some(command_id.clone());
        let request_id = request.request_id.clone();
        let error =
            tokio::time::timeout(std::time::Duration::from_secs(5), client.request(request))
                .await
                .unwrap()
                .unwrap_err();
        let error = format!("{error:#}");
        assert!(
            error.contains("OUTCOME_UNKNOWN")
                && error.contains(&command_id)
                && error.contains(&request_id)
                && error.contains("reading frame length"),
            "{error}"
        );
        let observed = server_task.await.unwrap();
        assert_eq!(observed["commandId"], command_id);
        assert_eq!(observed["requestId"], request_id);
        assert!(client.pending.lock().unwrap().pending.is_empty());
    }

    #[tokio::test]
    async fn named_pipe_cancelled_reply_does_not_strand_later_reads() {
        let name = format!(r"\\.\pipe\AgentCenter-cancelled-{}", Uuid::new_v4());
        let mut server = create_pipe(&name, true).unwrap();
        let (received, receipt) = oneshot::channel();
        let (release, released) = oneshot::channel();
        let (finish, finished) = oneshot::channel();
        let server_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            negotiate(&mut server, "store", "instance").await.unwrap();
            let first = read_frame(&mut server).await.unwrap();
            received.send(()).unwrap();
            released.await.unwrap();
            write_frame(
                &mut server,
                &json!({"type":"response","requestId":first["requestId"],
                "status":"ok","data":{},"subjects":[]}),
            )
            .await
            .unwrap();
            let second = read_frame(&mut server).await.unwrap();
            assert_eq!(second["method"], "project.list");
            write_frame(
                &mut server,
                &json!({"type":"response","requestId":second["requestId"],
                "status":"ok","data":{},"subjects":[]}),
            )
            .await
            .unwrap();
            finished.await.unwrap();
        });
        let client = Arc::new(Client::connect_to(&name).await.unwrap());
        let worker = client.clone();
        let caller =
            tokio::spawn(async move { worker.request(Request::new("work.list", json!({}))).await });
        tokio::time::timeout(std::time::Duration::from_secs(5), receipt)
            .await
            .unwrap()
            .unwrap();
        drop(client.writer.lock().await);
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(client.pending.lock().unwrap().pending.is_empty());
        release.send(()).unwrap();
        assert_eq!(
            client
                .request(Request::new("project.list", json!({})))
                .await
                .unwrap()
                .status,
            "ok"
        );
        finish.send(()).unwrap();
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn named_pipe_reconnect_refuses_a_different_store() {
        let name = format!(r"\\.\pipe\AgentCenter-store-change-{}", Uuid::new_v4());
        let mut first = create_pipe(&name, true).unwrap();
        let first_task = tokio::spawn(async move {
            first.connect().await.unwrap();
            negotiate(&mut first, "original-store", "instance")
                .await
                .unwrap();
        });
        let client = Client::connect_to(&name).await.unwrap();
        first_task.await.unwrap();
        let mut second = create_pipe(&name, false).unwrap();
        let second_task = tokio::spawn(async move {
            second.connect().await.unwrap();
            negotiate(&mut second, "different-store", "instance")
                .await
                .unwrap();
        });
        assert!(client
            .reconnect()
            .await
            .err()
            .unwrap()
            .to_string()
            .contains("STORE_CHANGED"));
        second_task.await.unwrap();
    }

    #[tokio::test]
    async fn named_pipe_recovery_installs_snapshot_then_replays_committed_events() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("wta-center-recovery-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let handle = start_engine(root.clone()).await.unwrap();
        let runtime_id = Uuid::new_v4().to_string();
        let mut registration = Request::new(
            "runtime.register",
            json!({
                "runtimeInstanceId":runtime_id,"protocolVersions":[1],"capabilities":[
                    {"id":"recovery-fixture","kinds":["ProduceResult","Coordinate"],"supportsContinuation":true,"supportsScopedStop":true},
                    {"id":"native-check","kinds":["EvaluateGate"],"supportsContinuation":false,"supportsScopedStop":true}
                ]
            }),
        );
        registration.command_id = Some(Uuid::new_v4().to_string());
        assert_eq!(
            handle
                .request(Principal::Runtime { runtime_id }, registration)
                .await
                .status,
            "ok"
        );
        let mut configure = Request::new(
            "project.configure",
            json!({
                "name":"Recovery fixture","root":root,
                "coordinatorCapabilityId":"recovery-fixture","workerCapabilityId":"recovery-fixture","checkCapabilityId":"native-check",
                "limits":{"concurrency":1,"executionAttempts":1,"evaluationAttempts":1,"coordinationTurns":1,
                    "contextRounds":1,"executionSeconds":30,"coordinationSeconds":30}
            }),
        );
        configure.command_id = Some(Uuid::new_v4().to_string());
        let configured = handle.request(Principal::Human, configure).await;
        assert_eq!(configured.status, "ok", "{configured:?}");
        let project_id = configured.data.unwrap()["projectId"].clone();
        let name = format!(r"\\.\pipe\AgentCenter-recovery-{}", Uuid::new_v4());
        let server = create_pipe(&name, true).unwrap();
        let service = handle.clone();
        let first_task = tokio::spawn(async move {
            server.connect().await.unwrap();
            connection(server, service, "recovery-instance".to_owned()).await
        });
        let client = Client::connect_to(&name).await.unwrap();
        let initial = client
            .request(Request::new(
                "events.subscribe",
                json!({"scope":{"kind":"WorkList"}}),
            ))
            .await
            .unwrap();
        let old_id = initial.data.as_ref().unwrap()["subscriptionId"].clone();
        let cursor = initial.cursor.unwrap();
        first_task.abort();
        assert!(first_task.await.unwrap_err().is_cancelled());
        let mut disconnected = client.event_status.clone();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            disconnected.wait_for(|status| matches!(status, EventStatus::Closed(_))),
        )
        .await
        .unwrap()
        .unwrap();
        // Recover only presentation subscriptions, never a submitted mutation.
        let second = create_pipe(&name, false).unwrap();
        let service = handle.clone();
        let second_task = tokio::spawn(async move {
            second.connect().await.unwrap();
            connection(second, service, "recovery-instance".to_owned()).await
        });
        let recovered = client.reconnect().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let mut draft = Request::new("work.create_draft", json!({
                "projectId":project_id,"goal":"Retain committed recovery evidence",
                "scope":["report.txt"],"exclusions":[],"context":[],"sourceMessageIds":[],
                "criteria":[{"id":"report","description":"A report exists","evidenceRule":"artifact:report"}],
                "delivery":{"kind":"Report"}
            }));
            draft.command_id = Some(Uuid::new_v4().to_string());
            let receipt = recovered.request(draft).await?;
            anyhow::ensure!(receipt.status == "ok", "{receipt:?}");
            let snapshot = recovered.request(Request::new("events.subscribe",
                json!({"scope":{"kind":"WorkList"}}))).await?;
            anyhow::ensure!(snapshot.status == "ok", "{snapshot:?}");
            let snapshot = snapshot.data.unwrap();
            anyhow::ensure!(snapshot["subscriptionId"] != old_id);
            anyhow::ensure!(snapshot["snapshot"]["items"].as_array().is_some_and(|items| items.len() == 1),
                "fresh snapshot omitted committed work: {snapshot}");
            let replay = recovered.request(Request::new("events.subscribe",
                json!({"scope":{"kind":"WorkList"},"afterCursor":cursor}))).await?;
            anyhow::ensure!(replay.status == "ok", "{replay:?}");
            let replay_id = replay.data.unwrap()["subscriptionId"].clone();
            loop {
                let event = recovered.next_event().await?;
                anyhow::ensure!(event["subscriptionId"] == replay_id && event["subscriptionId"] != old_id,
                    "old scope leaked across reconnect: {event}");
                if event["cursor"].as_str() == receipt.cursor.as_deref() {
                    break;
                }
            }
            Ok::<(), anyhow::Error>(())
        }).await;
        drop(client);
        drop(recovered);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), second_task)
            .await
            .unwrap()
            .unwrap();
        handle.shutdown().await.unwrap();
        tokio::fs::remove_dir_all(root).await.unwrap();
        result.unwrap().unwrap();
    }
}
