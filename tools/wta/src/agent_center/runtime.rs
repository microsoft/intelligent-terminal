//! Headless execution and immutable filesystem effects for Agent Center.
//!
//! Human-installed `adapters.json` entries require `adapter.approvedModelDestination`,
//! a nonempty description of the approved provider/account or endpoint. This is
//! explicit approval metadata, never inferred from inherited environment variables;
//! it is not a network-isolation or provider-endpoint verification guarantee.

mod acp;
pub(super) mod artifacts;
mod bridge;
#[cfg(test)]
mod conformance;
mod executor;
mod process;
mod workspace;

use super::engine::Effect;
use super::wire::{Principal, Request, Response};
use super::ServiceHandle;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Read only verified captured members, never arbitrary provider or workspace paths.
pub(super) fn read_artifact(
    record: &Value,
    relative_path: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<Value> {
    anyhow::ensure!(
        (1..=32_768).contains(&limit),
        "read limit must be 1..32768 bytes"
    );
    anyhow::ensure!(record["availability"] == "Ready", "artifact is not ready");
    let root = artifacts::verify(record)?;
    let entries = record["manifest"]["entries"]
        .as_array()
        .context("captured manifest entries missing")?;
    if record["manifest"]["kind"] == "Tree" && relative_path.is_none() {
        let start = usize::try_from(offset).context("manifest offset out of range")?;
        anyhow::ensure!(start <= entries.len(), "manifest offset out of range");
        let mut page = Vec::new();
        let mut bytes = 2;
        for entry in &entries[start..] {
            let length = serde_json::to_vec(entry)?.len() + 1;
            if bytes + length > limit as usize {
                anyhow::ensure!(
                    !page.is_empty(),
                    "limit is too small for one manifest entry"
                );
                break;
            }
            bytes += length;
            page.push(entry.clone());
        }
        let next = start + page.len();
        return Ok(json!({"artifactId":record["id"],"digest":record["digest"],
            "kind":"Tree","offsetUnit":"entries","offset":offset,"entries":page,
            "nextOffset":next,"eof":next == entries.len()}));
    }
    let entry = if let Some(relative) = relative_path {
        // Manifest membership is required in addition to path containment.
        entries
            .iter()
            .find(|entry| entry["path"].as_str() == Some(relative))
            .context("relativePath is not a captured member")?
    } else {
        anyhow::ensure!(
            record["manifest"]["kind"] == "File" && entries.len() == 1,
            "a captured file member must be selected explicitly"
        );
        &entries[0]
    };
    anyhow::ensure!(
        entry["kind"] != "Directory",
        "selected member is a directory"
    );
    let relative = text(entry, "path")?;
    let content = std::fs::read(artifacts::resolve(&root, relative)?)?;
    anyhow::ensure!(
        artifacts::digest(&content) == entry["digest"]
            && Some(content.len() as u64) == entry["size"].as_u64(),
        "captured member changed while reading"
    );
    let start = usize::try_from(offset).context("byte offset out of range")?;
    anyhow::ensure!(start <= content.len(), "byte offset out of range");
    let mut result = json!({"artifactId":record["id"],"digest":record["digest"],
        "relativePath":relative,"sizeBytes":content.len(),"offsetUnit":"bytes","offset":offset});
    let text = match std::str::from_utf8(&content) {
        Ok(text) if !text.contains('\0') => text,
        _ => {
            result["kind"] = json!("Binary");
            result["encoding"] = json!("unsupported");
            result["reason"] =
                json!("Binary evidence is retained but cannot be interpreted by this text reader");
            return Ok(result);
        }
    };
    anyhow::ensure!(
        text.is_char_boundary(start),
        "offset must be a UTF-8 character boundary"
    );
    let mut end = start.saturating_add(limit as usize).min(content.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    anyhow::ensure!(
        end > start || start == content.len(),
        "limit is too small for one UTF-8 character"
    );
    result["kind"] = json!("Text");
    result["encoding"] = json!("utf-8");
    result["text"] = json!(&text[start..end]);
    result["nextOffset"] = json!(end);
    result["eof"] = json!(end == content.len());
    Ok(result)
}

#[derive(Clone)]
pub struct Runtime {
    inner: Arc<Inner>,
}

struct Inner {
    handle: ServiceHandle,
    root: PathBuf,
    invocations: Mutex<HashMap<String, Arc<Invocation>>>,
    invocation_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    effects: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    adapters: BTreeMap<String, Value>,
    conversation_policy: Option<Value>,
    runtime_id: String,
    shutting_down: std::sync::atomic::AtomicBool,
}

struct Invocation {
    input: Value,
    state: Mutex<InvocationState>,
    report_lock: Mutex<()>,
    cancel: CancellationToken,
    commands: mpsc::Sender<Control>,
}

enum Control {
    Continue(Value),
    WorkInput(Value),
    Release,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct InvocationState {
    state: String,
    sequence: u64,
    #[serde(default)]
    pending_observation: Option<Request>,
    turn: u64,
    terminal_record_ids: Vec<String>,
    terminal_observation: Option<Value>,
    continuation_ids: BTreeMap<String, Value>,
    current_continuation_id: Option<String>,
    stop_operations: Vec<String>,
    acknowledged: bool,
    waiting: bool,
    released: bool,
    settled: bool,
    execution_identity: String,
    #[serde(default)]
    primary_session: Option<PrimarySession>,
    #[serde(skip)]
    owned_job: Option<Arc<process::ProcessJob>>,
    #[serde(default)]
    settlement_proof: Option<String>,
    #[serde(default)]
    execution_kind: String,
    #[serde(default)]
    work_inputs: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrimarySession {
    work_id: String,
    capability_id: String,
    provider_configuration_digest: String,
    provider_session_id: String,
    cwd: PathBuf,
}

impl Runtime {
    pub fn new(handle: ServiceHandle, root: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&root).context("create Agent Center runtime root")?;
        std::fs::create_dir_all(root.join("invocations"))?;
        std::fs::create_dir_all(root.join("effects"))?;
        let configuration = root.join("adapters.json");
        let (adapters, conversation_policy) = if configuration.exists() {
            let bytes = std::fs::read(configuration)?;
            let adapters = read_adapters(&bytes)?;
            let policy = read_conversation_policy(&bytes, &adapters)?;
            (adapters, policy)
        } else {
            (BTreeMap::new(), None)
        };
        Ok(Self {
            inner: Arc::new(Inner {
                handle,
                root,
                invocations: Mutex::new(HashMap::new()),
                invocation_threads: Mutex::new(Vec::new()),
                effects: Mutex::new(HashMap::new()),
                adapters,
                conversation_policy,
                runtime_id: Uuid::new_v4().to_string(),
                shutting_down: std::sync::atomic::AtomicBool::new(false),
            }),
        })
    }

    /// Advertise only configured ACP adapters; loading configuration never launches a model.
    pub async fn register(&self) -> Result<()> {
        let mut capabilities = vec![
            json!({"id":"native-check","kinds":["EvaluateGate"],"supportsContinuation":false,"supportsScopedStop":true}),
        ];
        capabilities.extend(self.inner.adapters.keys().map(|id| {
            json!({
                "id":id,"kinds":["ProduceResult","ReviewResult","Coordinate","ExecuteWork"],
                "supportsContinuation":true,"supportsScopedStop":true
            })
        }));
        success(self.request(Principal::Runtime {runtime_id:self.inner.runtime_id.clone()}, "runtime.register", json!({
            "runtimeInstanceId":self.inner.runtime_id,"protocolVersions":[1],"capabilities":capabilities
        })).await)?;
        if let Some(policy) = &self.inner.conversation_policy {
            success(
                self.request(Principal::Service, "console.configure", policy.clone())
                    .await,
            )?;
        }
        Ok(())
    }

    /// Settle owned process trees and join executors before their store/files may close.
    pub async fn shutdown(&self) -> Result<()> {
        self.inner
            .shutting_down
            .store(true, std::sync::atomic::Ordering::Release);
        let invocations: Vec<_> = self
            .inner
            .invocations
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for invocation in &invocations {
            invocation.cancel.cancel();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        loop {
            let mut unsettled = Vec::new();
            for invocation in &invocations {
                if !invocation.state.lock().await.settled {
                    unsettled.push(text(&invocation.input, "id")?.to_owned());
                }
            }
            let mut threads = self.inner.invocation_threads.lock().await;
            if unsettled.is_empty() && threads.iter().all(std::thread::JoinHandle::is_finished) {
                for thread in threads.drain(..) {
                    thread.join().map_err(|_| {
                        anyhow::anyhow!("invocation executor thread panicked during shutdown")
                    })?;
                }
                return Ok(());
            }
            let pending_threads = threads
                .iter()
                .filter(|thread| !thread.is_finished())
                .count();
            drop(threads);
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "runtime shutdown could not finish: unsettled invocations [{}], {pending_threads} executor threads still running",
                    unsettled.join(", ")
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn execute(&self, effect: Effect) -> Result<()> {
        Uuid::parse_str(&effect.id).context("effect ID must be UUID")?;
        let serial = self
            .inner
            .effects
            .lock()
            .await
            .entry(effect.id.clone())
            .or_default()
            .clone();
        let _serial = serial.lock().await;
        let intent = self
            .inner
            .root
            .join("effects")
            .join(format!("{}.intent", effect.id));
        let fingerprint = artifacts::digest(&serde_json::to_vec(
            &json!({"method":effect.method,"params":effect.params}),
        )?);
        if intent.exists() && tokio::fs::read_to_string(&intent).await? != fingerprint {
            bail!("effect ID reused with different content");
        }
        let receipt = self
            .inner
            .root
            .join("effects")
            .join(format!("{}.response.json", effect.id));
        if receipt.exists() {
            let response = serde_json::from_slice(&tokio::fs::read(receipt).await?)?;
            return self
                .inner
                .handle
                .complete_effect(&effect.id, response)
                .await;
        }
        if intent.exists() && !effect.method.starts_with("runtime.") {
            let response = Response::fail(&effect.id, "OUTCOME_UNKNOWN", "filesystem effect was previously started without a completion receipt; reconcile the same operation");
            return self
                .inner
                .handle
                .complete_effect(&effect.id, response)
                .await;
        }
        tokio::fs::write(intent, fingerprint).await?;
        let result = self.effect(&effect.method, &effect.params).await;
        let mut response = match result {
            Ok(response) => response,
            Err(error) => Response::fail(&effect.id, "EXECUTION_FAILED", format!("{error:#}")),
        };
        response.request_id = effect.id.clone();
        tokio::fs::write(receipt, serde_json::to_vec(&response)?)
            .await
            .context("persist effect completion receipt")?;
        self.inner
            .handle
            .complete_effect(&effect.id, response)
            .await
    }

    async fn effect(&self, method: &str, params: &Value) -> Result<Response> {
        match method {
            "runtime.recover_coordinator" => Ok(Response::ok(
                "",
                serde_json::to_value(self.recover_coordinator_session(params).await?)?,
            )),
            "project.create" => Ok(Response::ok("", workspace::create_project(params).await?)),
            "runtime.invoke" => self.invoke(params["invocation"].clone()).await,
            "runtime.work_input" => self.work_input(params).await,
            "runtime.continue" => {
                let invocation = self.lookup(params).await?;
                let continuation = &params["continuation"];
                let id = text(continuation, "id")?;
                let mut state = invocation.state.lock().await;
                if let Some(original) = state.continuation_ids.get(id) {
                    if original != continuation {
                        bail!("continuation ID reused with different content");
                    }
                    return Ok(Response::ok(
                        "",
                        json!({"continuationId":id,"disposition":"AlreadyRecorded"}),
                    ));
                }
                if state.released || state.state == "Ended" || invocation.cancel.is_cancelled() {
                    return Ok(Response::fail(
                        "",
                        "BAD_STATE",
                        "invocation cannot be continued",
                    ));
                }
                let dispatch = &invocation.input["dispatch"];
                for key in ["dispatchId", "taskRevision", "bindingGeneration"] {
                    let expected = match key {
                        "dispatchId" => &dispatch["id"],
                        "taskRevision" => &dispatch["taskRevision"],
                        _ => &invocation.input["bindingGeneration"],
                    };
                    if &continuation[key] != expected {
                        bail!("continuation binding mismatch: {key}");
                    }
                    if continuation["compatibleInputManifestDigest"]
                        != dispatch["inputManifestDigest"]
                    {
                        bail!("continuation cannot change the pinned input manifest");
                    }
                }
                let limit = invocation.input["limits"]["remainingContextRounds"]
                    .as_u64()
                    .unwrap_or(0);
                if state.continuation_ids.len() as u64 >= limit {
                    bail!("context allowance exhausted");
                }
                state
                    .continuation_ids
                    .insert(id.into(), continuation.clone());
                self.persist(&invocation, &state)?;
                invocation
                    .commands
                    .try_send(Control::Continue(continuation.clone()))
                    .context("continuation queue full")?;
                Ok(Response::ok(
                    "",
                    json!({"continuationId":id,"disposition":"Recorded"}),
                ))
            }
            "runtime.stop" => {
                let invocation = match self.lookup(params).await {
                    Ok(invocation) => invocation,
                    Err(_) => match self.restore_settled_invocation(params).await {
                        Ok(invocation) => invocation,
                        Err(error) => {
                            return Ok(Response::fail("", "OUTCOME_UNKNOWN", format!("{error:#}")))
                        }
                    },
                };
                let operation = text(params, "operationId")?;
                let job = {
                    let mut state = invocation.state.lock().await;
                    if !state.stop_operations.iter().any(|id| id == operation) {
                        state.stop_operations.push(operation.into());
                    }
                    invocation.cancel.cancel();
                    let job = state.owned_job.clone();
                    if !state.settled {
                        state.state = "Settling".into();
                    }
                    self.persist(&invocation, &state)?;
                    job
                };
                if let Some(job) = job {
                    // The job handle, not a PID lookup, identifies every descendant owned
                    // by this invocation even when its ACP transport/terminal report stalled.
                    job.terminate()?;
                    job.settle().await?;
                    self.record_settlement(&invocation).await?;
                }
                if invocation.state.lock().await.settled {
                    self.reconcile_settled_observation(&invocation, params)
                        .await?;
                    return Ok(Response::ok(
                        "",
                        json!({"quiescent":true,"operationId":operation}),
                    ));
                }
                Ok(Response::pending(
                    "",
                    operation.into(),
                    json!({"invocationId":params["invocationId"]}),
                ))
            }
            "runtime.release" => {
                let invocation = match self.lookup(params).await {
                    Ok(invocation) => invocation,
                    Err(_) => match self.restore_settled_invocation(params).await {
                        Ok(invocation) => invocation,
                        Err(error) => {
                            return Ok(Response::fail("", "OUTCOME_UNKNOWN", format!("{error:#}")))
                        }
                    },
                };
                let mut state = invocation.state.lock().await;
                if !state.settled || !matches!(state.state.as_str(), "Idle" | "Ended") {
                    return Ok(Response::fail(
                        "",
                        "BAD_STATE",
                        "release requires proven quiescence",
                    ));
                }
                if !state.released {
                    if let Err(error) = invocation.commands.try_send(Control::Release) {
                        tracing::debug!(target:"agent_center", %error, "settled invocation control channel is closed");
                    }
                    state.released = true;
                    state.state = "Ended".into();
                    state.owned_job = None;
                    invocation.cancel.cancel();
                    self.persist(&invocation, &state)?;
                }
                Ok(Response::ok("", json!({"released":true})))
            }
            "runtime.probe" => {
                let id = text(params, "invocationId")?;
                let entries = self.inner.invocations.lock().await;
                let Some(invocation) = entries.get(id) else {
                    let state = if self.ledger(id)?.exists() {
                        "Unknown"
                    } else {
                        "NotStarted"
                    };
                    return Ok(Response::ok(
                        "",
                        json!({"state":state,"lastSequence":0,"terminalRecordIds":[]}),
                    ));
                };
                let state = invocation.state.lock().await;
                let mut data = json!({"state":state.state,"lastSequence":state.sequence,"terminalRecordIds":state.terminal_record_ids});
                if let Some(observation) = &state.terminal_observation {
                    data["terminalObservation"] = observation.clone();
                }
                Ok(Response::ok("", data))
            }
            "artifact.capture" => {
                let path = self.workspace(text(params, "workspaceId")?).await?;
                let sources = params["sources"]
                    .as_array()
                    .context("capture sources required")?;
                if sources.is_empty() || sources.len() > 32 {
                    bail!("capture requires 1..32 sources");
                }
                let mut captured = Vec::new();
                for source in sources {
                    captured.push(self.capture(&path, source).await?);
                }
                Ok(Response::ok("", json!({"artifacts":captured})))
            }
            "workspace.create" | "workspace.provision" => Ok(Response::ok(
                "",
                workspace::create(&self.inner.root, params).await?,
            )),
            "workspace.prepare_delivery" | "workspace.commit" => {
                let path = self.workspace(text(params, "workspaceId")?).await?;
                let data =
                    workspace::commit(&self.inner.root, &path, "Agent Center fixed local delivery")
                        .await?;
                Ok(Response::ok("", data))
            }
            "workspace.handback" => {
                let path = self.workspace(text(params, "workspaceId")?).await?;
                workspace::managed(&self.inner.root, &path)?;
                let artifact = self
                    .capture(&path, &json!({"kind":"Tree","relativePath":""}))
                    .await?;
                Ok(Response::ok(
                    "",
                    json!({"artifacts":[artifact],"workspaceId":params["workspaceId"],"summary":params["summary"]}),
                ))
            }
            "workspace.takeover" => {
                // The service holds admission and settles writers before dispatching this effect.
                let path = self.workspace(text(params, "workspaceId")?).await?;
                workspace::managed(&self.inner.root, &path)?;
                Ok(Response::ok(
                    "",
                    json!({"workspaceId":params["workspaceId"],"localPath":path,"ready":true}),
                ))
            }
            _ => Ok(Response::fail(
                "",
                "METHOD_UNSUPPORTED",
                format!("unsupported runtime effect {method}"),
            )),
        }
    }

    fn ledger(&self, id: &str) -> Result<PathBuf> {
        Uuid::parse_str(id).context("invocation ID must be a UUID")?;
        Ok(self
            .inner
            .root
            .join("invocations")
            .join(format!("{id}.json")))
    }

    fn persist(&self, invocation: &Invocation, state: &InvocationState) -> Result<()> {
        let path = self.ledger(text(&invocation.input, "id")?)?;
        // The ledger intentionally stores no provider configuration or MCP bearer.
        std::fs::write(path, serde_json::to_vec(&json!({"inputDigest":artifacts::digest(&serde_json::to_vec(&invocation.input)?),
            "binding":{"invocationId":invocation.input["id"],"runtimeId":invocation.input["runtimeId"],
                "bindingGeneration":invocation.input["bindingGeneration"]},"state":state}))?)
            .context("persist invocation ledger")
    }

    async fn restore_settled_invocation(&self, params: &Value) -> Result<Arc<Invocation>> {
        let id = text(params, "invocationId")?;
        let expected = &params["reconciliation"];
        let ledger: Value =
            serde_json::from_slice(&tokio::fs::read(self.ledger(id)?).await.context(
                "The invocation is not owned by this runtime and has no settlement ledger",
            )?)?;
        let mut state: InvocationState = serde_json::from_value(ledger["state"].clone())?;
        anyhow::ensure!(
            state.settled
                && (state.settlement_proof.as_deref() == Some(state.execution_identity.as_str())
                    || state
                        .terminal_observation
                        .as_ref()
                        .is_some_and(|observation| observation["data"]["quiescent"] == true)),
            "No durable proof of process-tree settlement; a missing provider PID is not sufficient"
        );
        anyhow::ensure!(
            !state.execution_identity.is_empty()
                && expected["executionIdentity"].as_str()
                    == Some(state.execution_identity.as_str()),
            "Settlement ledger does not match the tracked execution identity"
        );
        let binding = json!({"invocationId":id,"runtimeId":expected["runtimeId"],
            "bindingGeneration":expected["bindingGeneration"]});
        anyhow::ensure!(
            ledger.get("binding").is_none_or(|saved| saved == &binding),
            "Settlement ledger belongs to another invocation binding"
        );
        let runtime_id = text(expected, "runtimeId")?;
        let generation = expected["bindingGeneration"]
            .as_u64()
            .filter(|generation| *generation > 0)
            .context("Missing authoritative binding generation for reconciliation")?;
        state.state = "Idle".into();
        let (commands, _receiver) = mpsc::channel(1);
        let invocation = Arc::new(Invocation {
            input: json!({"id":id,"runtimeId":runtime_id,"bindingGeneration":generation}),
            state: Mutex::new(state),
            report_lock: Mutex::new(()),
            cancel: CancellationToken::new(),
            commands,
        });
        invocation.cancel.cancel();
        let mut invocations = self.inner.invocations.lock().await;
        Ok(invocations
            .entry(id.to_owned())
            .or_insert(invocation)
            .clone())
    }

    async fn reconcile_settled_observation(
        &self,
        invocation: &Invocation,
        params: &Value,
    ) -> Result<()> {
        {
            let _serial = invocation.report_lock.lock().await;
            let mut state = invocation.state.lock().await;
            anyhow::ensure!(state.settled, "Execution settlement has not been proven");
            if let Some(expected) = params.get("reconciliation") {
                anyhow::ensure!(
                    expected["runtimeId"] == invocation.input["runtimeId"]
                        && expected["bindingGeneration"] == invocation.input["bindingGeneration"],
                    "Reconciliation targets another runtime binding"
                );
                let expected_identity = text(expected, "executionIdentity")?;
                anyhow::ensure!(
                    expected_identity.is_empty() || expected_identity == state.execution_identity,
                    "Reconciliation targets another execution identity"
                );
                self.flush_observation(invocation, &mut state).await?;
                state.sequence = state.sequence.max(
                    expected["lastSequence"]
                        .as_u64()
                        .context("Missing authoritative observation sequence")?,
                );
                for operation in expected["stopOperationIds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                {
                    if !state
                        .stop_operations
                        .iter()
                        .any(|existing| existing == operation)
                    {
                        state.stop_operations.push(operation.to_owned());
                    }
                }
            }
            state.state = "Idle".into();
            self.persist(invocation, &state)?;
        }
        self.settled(invocation).await
    }

    async fn record_settlement(&self, invocation: &Invocation) -> Result<()> {
        let mut state = invocation.state.lock().await;
        state.settled = true;
        state.settlement_proof = Some(state.execution_identity.clone());
        self.persist(invocation, &state)
    }

    async fn invoke(&self, mut input: Value) -> Result<Response> {
        if input["dispatch"]["kind"] != "EvaluateGate" {
            let capability = text(&input, "capabilityId")?;
            let adapter = self
                .inner
                .adapters
                .get(capability)
                .context("ACP capability is not configured in adapters.json")?;
            input["adapter"] = adapter.clone();
        }
        let id = text(&input, "id")?.to_owned();
        let mut invocations = self.inner.invocations.lock().await;
        if self
            .inner
            .shutting_down
            .load(std::sync::atomic::Ordering::Acquire)
        {
            bail!("runtime is shutting down; invocation admission is closed");
        }
        if let Some(existing) = invocations.get(&id) {
            let mut original = existing.input.clone();
            if let Some(fields) = original.as_object_mut() {
                fields.remove("resumeSession");
                fields.remove("resumeFailure");
            }
            return Ok(duplicate_receipt(&original, &input));
        }
        if self.ledger(&id)?.exists() {
            return Ok(Response::fail(
                "",
                "OUTCOME_UNKNOWN",
                "recorded invocation needs reconciliation; it will not be prompted twice",
            ));
        }
        if let Some(reference) = input.get("sessionReuseRef").and_then(Value::as_str) {
            let resumed = self.saved_primary_session(reference, &input);
            match resumed {
                Ok(session) => input["resumeSession"] = serde_json::to_value(session)?,
                Err(error) => {
                    input["resumeFailure"] = json!(format!("SESSION_RESUME_UNAVAILABLE: {error:#}"))
                }
            }
        }
        let work = input
            .pointer("/coordinationInput/scope/workId")
            .or_else(|| input.pointer("/executorInput/workId"))
            .and_then(Value::as_str);
        if let Some(work) = work {
            for existing in invocations.values() {
                if existing
                    .input
                    .pointer("/coordinationInput/scope/workId")
                    .or_else(|| existing.input.pointer("/executorInput/workId"))
                    .and_then(Value::as_str)
                    == Some(work)
                    && !existing.state.lock().await.released
                {
                    return Ok(Response::fail(
                        "",
                        "SESSION_BUSY",
                        "The work's primary execution has not been released",
                    ));
                }
            }
        }
        if invocations
            .values()
            .filter(|entry| !entry.cancel.is_cancelled())
            .count()
            >= 64
        {
            bail!("runtime invocation limit reached");
        }
        let (commands, receiver) = mpsc::channel(32);
        let invocation = Arc::new(Invocation {
            input,
            state: Mutex::new(InvocationState {
                state: "NotStarted".into(),
                settled: true,
                ..Default::default()
            }),
            report_lock: Mutex::new(()),
            cancel: CancellationToken::new(),
            commands,
        });
        self.persist(&invocation, &*invocation.state.lock().await)?;
        invocations.insert(id.clone(), invocation.clone());
        let runtime = self.clone();
        let thread = std::thread::Builder::new().name(format!("center-{id}")).spawn(move || {
            let result = tokio::runtime::Builder::new_current_thread().enable_all().build();
            match result {
                Ok(executor) => {
                    tokio::task::LocalSet::new().block_on(&executor, async {
                        if let Err(error) = runtime.run(invocation.clone(), receiver).await {
                            tracing::warn!(target:"agent_center", %error, "invocation failed");
                            let quiescent = invocation.state.lock().await.settled;
                            let finish = if invocation.cancel.is_cancelled() { "Cancelled" } else { "Error" };
                            if let Err(report_error) = runtime.end(&invocation, finish, Some(format!("{error:#}")), quiescent).await {
                                tracing::error!(target:"agent_center", %report_error, "failed to report invocation failure");
                            }
                            if quiescent && !invocation.state.lock().await.execution_identity.is_empty() {
                                if let Err(report_error) = runtime.settled(&invocation).await {
                                    tracing::error!(target:"agent_center", %report_error, "failed to report invocation settlement");
                                }
                            }
                        }
                    });
                }
                Err(error) => tracing::error!(target:"agent_center", %error, "cannot initialize invocation executor"),
            }
        }).context("spawn invocation executor thread")?;
        self.inner.invocation_threads.lock().await.push(thread);
        Ok(Response::ok(
            "",
            json!({"invocationId":id,"disposition":"Recorded"}),
        ))
    }

    fn saved_primary_session(&self, reference: &str, input: &Value) -> Result<PrimarySession> {
        if input.get("executorInput").is_some() {
            return self.saved_executor_session(reference, input);
        }
        let ledger: Value = serde_json::from_slice(
            &std::fs::read(self.ledger(reference)?)
                .context("SESSION_RESUME_UNAVAILABLE: primary session ledger is missing")?,
        )?;
        let state: InvocationState = serde_json::from_value(ledger["state"].clone())?;
        let session = if self.recovered_session_path(reference)?.exists() {
            let recovered: Value =
                serde_json::from_slice(&std::fs::read(self.recovered_session_path(reference)?)?)?;
            anyhow::ensure!(
                recovered["inputDigest"] == ledger["inputDigest"],
                "SESSION_RESUME_UNAVAILABLE: recovered coordinator provenance changed"
            );
            serde_json::from_value(recovered["session"].clone())?
        } else {
            anyhow::ensure!(
                state.released && state.settled,
                "SESSION_RESUME_UNAVAILABLE: primary execution is not proven settled and released"
            );
            state
                .primary_session
                .context("SESSION_RESUME_UNAVAILABLE: primary session association is missing")?
        };
        Uuid::parse_str(&session.work_id)
            .context("SESSION_RESUME_UNAVAILABLE: saved work identity is malformed")?;
        anyhow::ensure!(
            !session.provider_session_id.trim().is_empty()
                && !session.capability_id.trim().is_empty()
                && session.cwd.is_absolute(),
            "SESSION_RESUME_UNAVAILABLE: saved provider session or cwd is malformed"
        );
        anyhow::ensure!(
            input
                .pointer("/coordinationInput/scope/workId")
                .and_then(Value::as_str)
                == Some(session.work_id.as_str())
                && input["capabilityId"].as_str() == Some(session.capability_id.as_str())
                && artifacts::digest(&serde_json::to_vec(&input["adapter"])?)
                    == session.provider_configuration_digest,
            "SESSION_RESUME_UNAVAILABLE: saved session work/provider configuration does not match"
        );
        Ok(session)
    }

    fn recovered_session_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.ledger(id)?.with_extension("coordinator-session.json"))
    }

    async fn recover_coordinator_session(&self, params: &Value) -> Result<PrimarySession> {
        let mut original = params["invocation"].clone();
        let id = text(&original, "id")?.to_owned();
        let capability = text(&original, "capabilityId")?.to_owned();
        anyhow::ensure!(
            original["subject"]["kind"] == "Coordination"
                && original.get("dispatch").is_none()
                && original["coordinationInput"]["scope"]["workId"] == params["workId"]
                && original["coordinationInput"]["snapshot"]["work"]["workspaceId"] == params["workspaceId"],
            "SESSION_RESUME_UNAVAILABLE: only the original non-writing work coordinator can be recovered"
        );
        anyhow::ensure!(
            !self.inner.invocations.lock().await.contains_key(&id),
            "SESSION_BUSY: the coordinator is still owned by this runtime"
        );
        let adapter = self
            .inner
            .adapters
            .get(&capability)
            .context("SESSION_RESUME_UNAVAILABLE: original provider is not configured")?;
        original["adapter"] = adapter.clone();
        let ledger: Value = serde_json::from_slice(&tokio::fs::read(self.ledger(&id)?).await?)?;
        if original.get("sessionReuseRef").is_some() {
            let saved: PrimarySession = serde_json::from_value(
                ledger["state"]["primarySession"].clone(),
            )
            .context("SESSION_RESUME_UNAVAILABLE: resumed coordinator association is missing")?;
            original["resumeSession"] = serde_json::to_value(saved)?;
        }
        anyhow::ensure!(
            ledger["inputDigest"] == artifacts::digest(&serde_json::to_vec(&original)?)
                && ledger["state"]["executionIdentity"] == params["executionIdentity"]
                && ledger["state"]["acknowledged"] == false,
            "SESSION_RESUME_UNAVAILABLE: original invocation/provider provenance does not match"
        );
        let cwd = if let Some(saved) = ledger
            .pointer("/state/primarySession")
            .filter(|saved| saved.is_object())
        {
            let saved: PrimarySession = serde_json::from_value(saved.clone())?;
            anyhow::ensure!(
                saved.provider_session_id == text(params, "providerSessionId")?
                    && saved.work_id == text(params, "workId")?
                    && saved.capability_id == capability
                    && saved.provider_configuration_digest
                        == artifacts::digest(&serde_json::to_vec(adapter)?),
                "SESSION_RESUME_UNAVAILABLE: saved coordinator association changed"
            );
            anyhow::ensure!(
                saved.cwd.canonicalize()? == saved.cwd,
                "SESSION_RESUME_UNAVAILABLE: original directory was retargeted"
            );
            saved.cwd
        } else {
            let cwd = self.workspace(text(params, "workspaceId")?).await?;
            workspace::managed(&self.inner.root, &cwd)?;
            let expected = self
                .inner
                .root
                .join("workspaces")
                .join(text(params, "workspaceId")?)
                .canonicalize()?;
            anyhow::ensure!(
                cwd == expected,
                "SESSION_RESUME_UNAVAILABLE: original workspace identity changed"
            );
            cwd
        };
        let cwd = cwd
            .canonicalize()
            .context("SESSION_RESUME_UNAVAILABLE: original directory is unavailable")?;
        let session = PrimarySession {
            work_id: text(params, "workId")?.to_owned(),
            capability_id: capability,
            provider_configuration_digest: artifacts::digest(&serde_json::to_vec(adapter)?),
            provider_session_id: text(params, "providerSessionId")?.to_owned(),
            cwd,
        };
        anyhow::ensure!(
            !session.provider_session_id.trim().is_empty(),
            "SESSION_RESUME_UNAVAILABLE: original provider session ID is missing"
        );
        let record = json!({"inputDigest":ledger["inputDigest"],"session":session});
        let path = self.recovered_session_path(&id)?;
        if path.exists() {
            let previous: Value = serde_json::from_slice(&tokio::fs::read(&path).await?)?;
            anyhow::ensure!(
                previous == record,
                "SESSION_RESUME_UNAVAILABLE: recovered session association cannot be replaced"
            );
        } else {
            let pending = path.with_extension("pending");
            tokio::fs::write(&pending, serde_json::to_vec(&record)?).await?;
            tokio::fs::rename(&pending, &path).await?;
        }
        Ok(session)
    }

    async fn lookup(&self, params: &Value) -> Result<Arc<Invocation>> {
        self.inner
            .invocations
            .lock()
            .await
            .get(text(params, "invocationId")?)
            .cloned()
            .context("invocation not recorded in this runtime")
    }

    async fn request(&self, principal: Principal, method: &str, params: Value) -> Response {
        let mut request = Request::new(method, params);
        if !super::schemas::is_read(method) {
            request.command_id = Some(Uuid::new_v4().to_string());
        }
        self.inner.handle.request(principal, request).await
    }

    async fn bound(&self, invocation: &Invocation, method: &str, params: Value) -> Response {
        self.request(
            Principal::Invocation {
                invocation_id: invocation.input["id"].as_str().unwrap_or_default().into(),
            },
            method,
            params,
        )
        .await
    }

    async fn report(&self, invocation: &Invocation, kind: &str, data: Value) -> Result<()> {
        let _serial = invocation.report_lock.lock().await;
        let mut state = invocation.state.lock().await;
        self.flush_observation(invocation, &mut state).await?;
        let sequence = state.sequence + 1;
        let observation_id = Uuid::new_v4().to_string();
        let params = json!({
            "observationId":observation_id,"invocationId":invocation.input["id"],
            "bindingGeneration":invocation.input["bindingGeneration"],"sequence":sequence,"kind":kind,"data":data
        });
        let mut request = Request::new("runtime.report", params);
        request.command_id = Some(observation_id);
        state.pending_observation = Some(request);
        // Cancellation can drop the reply after the authority commits. Retain the
        // exact command before sending so the next report replays, rather than skips it.
        self.flush_observation(invocation, &mut state).await
    }

    async fn flush_observation(
        &self,
        invocation: &Invocation,
        state: &mut InvocationState,
    ) -> Result<()> {
        let Some(request) = state.pending_observation.clone() else {
            return Ok(());
        };
        anyhow::ensure!(
            request.method == "runtime.report"
                && request.params["invocationId"] == invocation.input["id"]
                && request.params["bindingGeneration"] == invocation.input["bindingGeneration"],
            "Pending observation belongs to another invocation binding"
        );
        self.persist(invocation, state)?;
        let runtime_id = text(&invocation.input, "runtimeId")?.into();
        let response = self
            .inner
            .handle
            .request(Principal::Runtime { runtime_id }, request.clone())
            .await;
        if response.status != "ok"
            && response
                .failure
                .as_ref()
                .is_some_and(|failure| failure.code != "EXECUTION_FAILED")
        {
            // A definitive rejection did not consume this sequence. Do not make an
            // invalid observation prevent the subsequent failure/settlement report.
            state.pending_observation = None;
            self.persist(invocation, state)?;
        }
        success(response)?;
        let sequence = request.params["sequence"]
            .as_u64()
            .context("Pending observation has no sequence")?;
        state.sequence = state.sequence.max(sequence);
        if request.params["kind"] == "TurnEnded" {
            state.terminal_observation =
                Some(json!({"sequence":sequence,"data":request.params["data"]}));
        }
        state.pending_observation = None;
        self.persist(invocation, state)
    }

    async fn end(
        &self,
        invocation: &Invocation,
        finish: &str,
        error: Option<String>,
        quiescent: bool,
    ) -> Result<()> {
        let turn = {
            let mut state = invocation.state.lock().await;
            state.state = if quiescent { "Idle" } else { "Settling" }.into();
            state.turn = state.turn.max(1);
            state.turn
        };
        let mut data = json!({"turnNumber":turn,"finish":finish,"quiescent":quiescent});
        if let Some(error) = error {
            data["errorText"] = json!(error);
        }
        self.report(invocation, "TurnEnded", data).await
    }

    async fn settled(&self, invocation: &Invocation) -> Result<()> {
        let data = {
            let state = invocation.state.lock().await;
            json!({"quiescent":true,"executionIdentity":state.execution_identity,"completedOperationIds":state.stop_operations})
        };
        self.report(invocation, "Settled", data).await
    }

    async fn workspace(&self, id: &str) -> Result<PathBuf> {
        let data = success(
            self.request(
                Principal::Service,
                "workspace.get",
                json!({"workspaceId":id}),
            )
            .await,
        )?;
        let record = data.get("workspace").unwrap_or(&data);
        let path = record["localPath"]
            .as_str()
            .or_else(|| record["root"].as_str())
            .or_else(|| record["localRoot"].as_str())
            .context("workspace has no local root")?;
        PathBuf::from(path)
            .canonicalize()
            .context("workspace root unavailable")
    }

    async fn capture(&self, path: &Path, source: &Value) -> Result<Value> {
        if source["kind"] == "GitCommit" {
            workspace::capture_commit(&self.inner.root, path, source).await
        } else {
            let (root, path, source) = (self.inner.root.clone(), path.to_owned(), source.clone());
            tokio::task::spawn_blocking(move || artifacts::capture(&root, &path, &source)).await?
        }
    }

    async fn coordination_directory(root: &Path, invocation_id: &str) -> Result<PathBuf> {
        let path = root.join("coordination").join(invocation_id);
        tokio::fs::create_dir_all(&path)
            .await
            .context("create coordination working directory")?;
        tokio::fs::canonicalize(path)
            .await
            .context("resolve coordination working directory")
    }

    async fn run(
        &self,
        invocation: Arc<Invocation>,
        controls: mpsc::Receiver<Control>,
    ) -> Result<()> {
        if invocation.cancel.is_cancelled() {
            bail!("invocation cancelled before execution");
        }
        if let Some(failure) = invocation
            .input
            .get("resumeFailure")
            .and_then(Value::as_str)
        {
            bail!("{failure}");
        }
        deadline(&invocation.input)?;
        let dispatch = &invocation.input["dispatch"];
        let cwd = if let Some(workspace) = dispatch["workspaceId"].as_str() {
            self.workspace(workspace).await?
        } else if let Some(workspace) = invocation.input["executorInput"]["workspaceId"].as_str() {
            self.workspace(workspace).await?
        } else if let Some(workspace) =
            invocation.input["coordinationInput"]["snapshot"]["work"]["workspaceId"].as_str()
        {
            self.workspace(workspace).await?
        } else {
            if invocation.input.get("resumeSession").is_some() {
                let saved: PrimarySession =
                    serde_json::from_value(invocation.input["resumeSession"].clone())?;
                let root = tokio::fs::canonicalize(self.inner.root.join("coordination")).await?;
                let cwd = tokio::fs::canonicalize(&saved.cwd).await.context(
                    "SESSION_RESUME_UNAVAILABLE: saved coordination cwd no longer exists",
                )?;
                anyhow::ensure!(
                    cwd.starts_with(root),
                    "SESSION_RESUME_UNAVAILABLE: saved cwd is outside coordination storage"
                );
                cwd
            } else {
                Self::coordination_directory(&self.inner.root, text(&invocation.input, "id")?)
                    .await?
            }
        };
        let cwd = process::launch_directory(&cwd).await?;
        if invocation.input.get("resumeSession").is_some() {
            let saved: PrimarySession =
                serde_json::from_value(invocation.input["resumeSession"].clone())?;
            anyhow::ensure!(
                saved.cwd == cwd,
                "SESSION_RESUME_UNAVAILABLE: work cwd changed since primary session startup"
            );
        }
        let mut inputs = Vec::new();
        let mut check_records = Vec::new();
        if let Some(refs) = dispatch["inputs"].as_array() {
            for reference in refs {
                let artifact = &reference["artifact"];
                let data = success(
                    self.bound(
                        &invocation,
                        "artifact.get",
                        json!({"artifactId":artifact["artifactId"]}),
                    )
                    .await,
                )?;
                let record = data.get("artifact").unwrap_or(&data).clone();
                if record["digest"] != artifact["digest"] {
                    bail!("pinned input digest differs from captured artifact");
                }
                if dispatch["kind"] == "EvaluateGate" {
                    check_records.push((reference.clone(), record.clone()));
                }
                let destination = self
                    .inner
                    .root
                    .join("inputs")
                    .join(text(&invocation.input, "id")?)
                    .join(text(artifact, "artifactId")?);
                let target = destination.clone();
                tokio::task::spawn_blocking(move || {
                    if target.exists() {
                        artifacts::verify_materialized(&record, &target)
                    } else {
                        artifacts::materialize(&record, &target)
                    }
                })
                .await??;
                inputs.push(
                    json!({"slot":reference["slot"],"localPath":destination,"artifact":artifact}),
                );
            }
        }
        if dispatch["kind"] == "EvaluateGate" {
            let check_cwd = self
                .inner
                .root
                .join("checks")
                .join(text(&invocation.input, "id")?);
            let target = check_cwd.clone();
            let check_dispatch = dispatch.clone();
            tokio::task::spawn_blocking(move || {
                artifacts::check_working_copy(&check_dispatch, &check_records, &target)
            })
            .await??;
            self.run_check(&invocation, &cwd, &check_cwd).await?;
        } else {
            acp::run(self.clone(), invocation, cwd, inputs, controls).await?;
        }
        Ok(())
    }

    async fn run_check(&self, invocation: &Invocation, cwd: &Path, check_cwd: &Path) -> Result<()> {
        let dispatch = &invocation.input["dispatch"];
        let gate_id = text(dispatch, "gateDefinitionId")?;
        let gate = dispatch["gateDefinitions"]
            .as_array()
            .and_then(|gates| gates.iter().find(|gate| gate["id"] == gate_id))
            .context("declared gate definition unavailable")?;
        let recipe: process::Recipe = serde_json::from_value(gate["recipe"].clone())?;
        if recipe.evidence_parser_id != "process-exit-v1" {
            bail!("unregistered evidence parser");
        }
        if !matches!(
            recipe.environment_ref.as_str(),
            "clean" | "clean-v1" | "default" | "local-default"
        ) {
            bail!("unregistered command environment");
        }
        {
            let mut state = invocation.state.lock().await;
            state.execution_identity = format!("command:{}", text(&invocation.input, "id")?);
            state.state = "Running".into();
            state.turn = 1;
        }
        self.report(invocation, "Started", json!({"adapterKind":"Command","executionIdentity":format!("command:{}", text(&invocation.input,"id")?)})).await?;
        success(self.bound(invocation, "task.acknowledge", json!({"dispatchId":dispatch["id"],"taskRevision":dispatch["taskRevision"],"disposition":"Accepted"})).await)?;
        let mut recipe = recipe;
        recipe.timeout_seconds = recipe
            .timeout_seconds
            .min(deadline(&invocation.input)?.as_secs().max(1));
        invocation.state.lock().await.settled = false;
        let outcome = process::run(&recipe, check_cwd, invocation.cancel.clone()).await?;
        self.record_settlement(invocation).await?;
        let evidence_dir = cwd
            .join(".agent-center-evidence")
            .join(text(&invocation.input, "id")?);
        tokio::fs::create_dir_all(&evidence_dir).await?;
        tokio::fs::write(evidence_dir.join("stdout.txt"), &outcome.stdout).await?;
        tokio::fs::write(evidence_dir.join("stderr.txt"), &outcome.stderr).await?;
        let evidence = json!({"recipe":recipe,"cwd":check_cwd,"inputManifestDigest":dispatch["inputManifestDigest"],"outcome":outcome});
        tokio::fs::write(
            evidence_dir.join("process.json"),
            serde_json::to_vec_pretty(&evidence)?,
        )
        .await?;
        let relative = evidence_dir.strip_prefix(cwd)?.to_string_lossy();
        let capture = self.bound(invocation, "artifact.capture", json!({"workspaceId":dispatch["workspaceId"],"sources":[{"kind":"Tree","relativePath":relative}],"purpose":"Evidence"})).await;
        let captured = bridge::await_operation(self, invocation, capture).await?;
        tokio::fs::remove_dir_all(&evidence_dir)
            .await
            .context("remove captured check staging")?;
        let refs = artifact_refs(&captured)?;
        let submission = json!({
            "evaluationUnitId":dispatch["evaluationUnitId"],"dispatchId":dispatch["id"],
            "taskRevision":dispatch["taskRevision"],"subjectResultId":dispatch["subjectResultId"],
            "evaluationRound":dispatch["evaluationRound"],"gateDefinitionId":gate_id,
            "gateDefinitionRevision":gate["revision"],"inputManifestDigest":dispatch["inputManifestDigest"],
            "outcome":outcome.disposition(),"evidence":refs,
            "explanation":format!("process-exit-v1: exit={:?}; timeout={}; cancelled={}; truncated={}",outcome.exit_code,outcome.timed_out,outcome.cancelled,outcome.output_truncated)
        });
        let submitted = success(self.bound(invocation, "gate.submit", submission).await)?;
        if let Some(id) = submitted["gateResultId"].as_str() {
            invocation
                .state
                .lock()
                .await
                .terminal_record_ids
                .push(id.into());
        }
        self.end(
            invocation,
            if outcome.cancelled {
                "Cancelled"
            } else {
                "Normal"
            },
            None,
            true,
        )
        .await?;
        self.settled(invocation).await
    }
}

fn artifact_refs(data: &Value) -> Result<Vec<Value>> {
    Ok(data["artifacts"]
        .as_array()
        .context("capture result has no artifacts")?
        .iter()
        .map(|artifact| json!({"artifactId":artifact["artifactId"],"digest":artifact["digest"]}))
        .collect())
}

fn duplicate_receipt(original: &Value, repeated: &Value) -> Response {
    if original != repeated {
        Response::fail(
            "",
            "COMMAND_ID_REUSED",
            "invocation ID reused with different content",
        )
    } else {
        Response::ok(
            "",
            json!({"invocationId":original["id"],"disposition":"AlreadyRecorded"}),
        )
    }
}

pub(super) fn validate_adapter_configuration(bytes: &[u8]) -> Result<()> {
    let adapters = read_adapters(bytes)?;
    read_conversation_policy(bytes, &adapters).map(|_| ())
}

fn read_conversation_policy(
    bytes: &[u8],
    adapters: &BTreeMap<String, Value>,
) -> Result<Option<Value>> {
    let config: Value = serde_json::from_slice(bytes)?;
    let capability = if let Some(id) = config.get("conversationCapabilityId") {
        let id = id
            .as_str()
            .context("conversationCapabilityId must be a capability ID")?;
        anyhow::ensure!(
            adapters.contains_key(id),
            "conversationCapabilityId is not an approved configured adapter"
        );
        Some(id)
    } else if adapters.len() == 1 {
        adapters.keys().next().map(String::as_str)
    } else {
        None
    };
    let Some(capability) = capability else {
        anyhow::ensure!(
            config.get("conversationLimits").is_none(),
            "conversationCapabilityId must select an approved adapter before setting conversationLimits"
        );
        return Ok(None);
    };
    let limits = config
        .get("conversationLimits")
        .cloned()
        .unwrap_or_else(|| {
            json!({
                "concurrency":1,"executionAttempts":8,"evaluationAttempts":16,
                "coordinationTurns":64,"contextRounds":3,
                "executionSeconds":600,"coordinationSeconds":120
            })
        });
    let typed: super::wire::Limits =
        serde_json::from_value(limits.clone()).context("Invalid global conversation limits")?;
    super::engine::Engine::validate_limits(&typed)
        .map_err(|response| anyhow::anyhow!("Invalid global conversation limits: {response:?}"))?;
    Ok(Some(json!({
        "capabilityId":capability,"workerCapabilityId":capability,"checkCapabilityId":"native-check",
        "approvedModelDestination":adapters[capability]["approvedModelDestination"],
        "limits":limits
    })))
}

fn read_adapters(bytes: &[u8]) -> Result<BTreeMap<String, Value>> {
    if bytes.len() > 262_144 {
        bail!("adapter configuration exceeds 256 KiB");
    }
    let config: Value = serde_json::from_slice(bytes)?;
    let capabilities = config["capabilities"]
        .as_array()
        .context("adapters.json requires capabilities[]")?;
    if capabilities.len() > 16 {
        bail!("at most 16 ACP capabilities can be configured");
    }
    let mut adapters = BTreeMap::new();
    for capability in capabilities {
        let id = text(capability, "id")?;
        if id.is_empty() || id.len() > 128 || id == "native-check" {
            bail!("invalid or reserved ACP capability ID");
        }
        let adapter = &capability["adapter"];
        if adapter["kind"] != "ACP" || text(adapter, "executable")?.is_empty() {
            bail!("configured capability requires ACP kind and executable");
        }
        if text(adapter, "executable")?.contains('\0') {
            bail!("invalid executable");
        }
        let destination = text(adapter, "approvedModelDestination")
            .context("ACP configuration requires an explicitly human-approved model destination")?;
        if destination.trim().is_empty()
            || destination.len() > 1024
            || destination.chars().any(char::is_control)
        {
            bail!("approvedModelDestination must be a nonempty description of at most 1024 bytes without control characters");
        }
        if let Some(args) = adapter.get("args") {
            let args: Vec<String> =
                serde_json::from_value(args.clone()).context("adapter args must be strings")?;
            if args.len() > 128 || args.iter().any(|arg| arg.contains('\0')) {
                bail!("invalid adapter arguments");
            }
        }
        if let Some(model) = adapter.get("model") {
            if !model.is_string() {
                bail!("adapter model must be a string");
            }
        }
        if let Some(environment) = adapter.get("environment") {
            let environment: BTreeMap<String, String> = serde_json::from_value(environment.clone())
                .context("adapter environment must map names to strings")?;
            if environment.len() > 64
                || environment.iter().any(|(name, value)| {
                    name.is_empty() || name.contains(['\0', '=']) || value.contains('\0')
                })
            {
                bail!("invalid adapter environment");
            }
        }
        if adapters.insert(id.into(), adapter.clone()).is_some() {
            bail!("duplicate capability ID");
        }
    }
    Ok(adapters)
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .with_context(|| format!("{key} must be a string"))
}

fn success(response: Response) -> Result<Value> {
    if response.status != "ok" {
        bail!(
            "work service {}: {}",
            response.status,
            response
                .failure
                .map(|failure| format!("{}: {}", failure.code, failure.message))
                .unwrap_or_default()
        );
    }
    response.data.context("work service response data missing")
}

fn deadline(input: &Value) -> Result<Duration> {
    let deadline = time::OffsetDateTime::parse(
        text(&input["limits"], "deadlineUtc")?,
        &time::format_description::well_known::Rfc3339,
    )?;
    let remaining =
        deadline.unix_timestamp_nanos() - time::OffsetDateTime::now_utc().unix_timestamp_nanos();
    if remaining <= 0 {
        bail!("invocation deadline expired");
    }

    Ok(Duration::from_nanos(
        (remaining as u128).min(3_600_000_000_000) as u64,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn executor_shutdown_waits_for_reporting_threads_after_process_settlement() -> Result<()>
    {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("executor-shutdown-{}", Uuid::new_v4()));
        let handle = super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let path = root.join("late-report.log");
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(&path)?;
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let (started, ready) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(15)).unwrap();
            drop(file);
        });
        runtime.inner.invocation_threads.lock().await.push(thread);
        ready.await?;
        let shutdown = runtime.shutdown();
        tokio::pin!(shutdown);
        assert!(
            futures::poll!(&mut shutdown).is_pending(),
            "Proven process settlement must not bypass unfinished executor reporting"
        );
        release.send(())?;
        shutdown.await?;
        assert!(runtime.inner.invocation_threads.lock().await.is_empty());
        let file = std::fs::OpenOptions::new()
            .write(true)
            .share_mode(0)
            .open(&path)?;
        drop(file);
        handle.shutdown().await?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn interrupted_observation_replays_before_text_and_terminal_reports() -> Result<()> {
        use std::future::Future;
        use std::task::Poll;

        for committed in [false, true] {
            let root = std::env::current_dir()?
                .join("target")
                .join(format!("center-report-replay-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root)?;
            std::fs::write(
                root.join("adapters.json"),
                serde_json::to_vec(&json!({"capabilities":[{
                    "id":"fixture","adapter":{"kind":"ACP","executable":"must-not-start.exe",
                        "args":[],"approvedModelDestination":"Observation replay fixture"}
                }]}))?,
            )?;
            let handle = super::super::transport::start_engine(root.clone()).await?;
            let runtime = Runtime::new(handle.clone(), root.clone())?;
            let result = async {
                runtime.register().await?;
                let conversation = Uuid::new_v4().to_string();
                success(runtime.request(Principal::Human, "conversation.submit", json!({
                    "conversationId":conversation,"clientMessageId":Uuid::new_v4().to_string(),
                    "text":"Replay fixture","attachments":[],
                    "context":{"scope":"Global","consoleSessionId":Uuid::new_v4().to_string(),
                        "contextVersion":1}
                })).await)?;
                let effect = handle
                    .effects()
                    .await?
                    .into_iter()
                    .find(|effect| effect.method == "runtime.invoke")
                    .context("missing invocation")?;
                let input = effect.params["invocation"].clone();
                let id = text(&input, "id")?.to_owned();
                let message = input["replyMessageId"].clone();
                let (commands, _receiver) = mpsc::channel(1);
                let mut invocation = Invocation {
                    input,
                    state: Mutex::new(InvocationState {
                        state: "Running".into(),
                        turn: 1,
                        settled: true,
                        execution_identity: "fixture-owned-process".into(),
                        ..Default::default()
                    }),
                    report_lock: Mutex::new(()),
                    cancel: CancellationToken::new(),
                    commands,
                };
                handle
                    .complete_effect(
                        &effect.id,
                        Response::ok("", json!({"invocationId":id,"disposition":"Recorded"})),
                    )
                    .await?;
                runtime
                    .report(
                        &invocation,
                        "Started",
                        json!({
                            "adapterKind":"ACP","executionIdentity":"fixture-owned-process",
                            "providerSessionId":"fixture-session"
                        }),
                    )
                    .await?;
                let first =
                    json!({"messageId":message,"partId":"turn-1","chunkIndex":0,"text":"before "});
                let db = rusqlite::Connection::open(root.join("work.db"))?;
                if committed {
                    // Hold the database writer so the reporting future cannot receive
                    // its acknowledgement before we cancel it.
                    db.execute_batch("BEGIN IMMEDIATE")?;
                    let mut reporting = Box::pin(runtime.report(&invocation, "TextDelta", first));
                    std::future::poll_fn(|cx| {
                        assert!(reporting.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    drop(reporting);
                    db.execute_batch("ROLLBACK")?;
                    success(
                        runtime
                            .request(Principal::Human, "work.list", json!({"limit":10}))
                            .await,
                    )?;
                } else {
                    let observation_id = Uuid::new_v4().to_string();
                    let mut request = Request::new(
                        "runtime.report",
                        json!({
                            "observationId":observation_id,"invocationId":id,
                            "bindingGeneration":invocation.input["bindingGeneration"],
                            "sequence":2,"kind":"TextDelta","data":first
                        }),
                    );
                    request.command_id = Some(observation_id);
                    let mut state = invocation.state.lock().await;
                    state.pending_observation = Some(request);
                    runtime.persist(&invocation, &state)?;
                }
                let ledger: Value = serde_json::from_slice(&std::fs::read(runtime.ledger(&id)?)?)?;
                assert_eq!(ledger["state"]["sequence"], 1);
                assert_eq!(
                    ledger["state"]["pendingObservation"]["params"]["sequence"],
                    2
                );
                // Recovery must work from the persisted outbox, not just process memory.
                invocation.state = Mutex::new(serde_json::from_value(ledger["state"].clone())?);
                runtime
                    .report(
                        &invocation,
                        "TextDelta",
                        json!({
                            "messageId":message,"partId":"turn-1","chunkIndex":1,"text":"after"
                        }),
                    )
                    .await?;
                assert_eq!(invocation.state.lock().await.sequence, 3);
                assert!(invocation.state.lock().await.pending_observation.is_none());
                let invocation = Arc::new(invocation);
                let (events, receiver) = mpsc::channel(128);
                for text in ["batched ", "text"] {
                    events
                        .send(acp::TextEvent::Chunk {
                            turn: 2,
                            text: text.into(),
                        })
                        .await?;
                }
                let (flushed, acknowledged) = tokio::sync::oneshot::channel();
                events.send(acp::TextEvent::Flush(flushed)).await?;
                events
                    .send(acp::TextEvent::Chunk {
                        turn: 2,
                        text: " next".into(),
                    })
                    .await?;
                events
                    .send(acp::TextEvent::Chunk {
                        turn: 3,
                        text: "new turn".into(),
                    })
                    .await?;
                drop(events);
                acp::report_text_events(runtime.clone(), invocation.clone(), receiver).await?;
                acknowledged.await?;
                assert_eq!(invocation.state.lock().await.sequence, 6);
                let rejected = runtime
                    .report(
                        &invocation,
                        "TextDelta",
                        json!({
                            "messageId":message,"partId":"turn-1","chunkIndex":99,"text":"invalid"
                        }),
                    )
                    .await;
                assert!(
                    rejected.is_err(),
                    "Invalid chunk must not consume an observation sequence"
                );
                assert!(invocation.state.lock().await.pending_observation.is_none());
                runtime
                    .end(
                        &invocation,
                        "Cancelled",
                        Some("Fixture deadline".into()),
                        true,
                    )
                    .await?;
                let saved: String =
                    db.query_row("SELECT body FROM records WHERE id=?1", [&id], |r| r.get(0))?;
                let saved: Value = serde_json::from_str(&saved)?;
                assert_eq!(saved["lastSequence"], 7);
                assert_eq!(saved["state"], "Releasing");
                assert_eq!(saved["lastTurnEnd"]["quiescent"], true);
                let item: String = db.query_row(
                    "SELECT body FROM records WHERE id=?1",
                    [message.as_str().context("fixture reply missing")?],
                    |r| r.get(0),
                )?;
                let item: Value = serde_json::from_str(&item)?;
                assert_eq!(item["parts"].as_array().unwrap().len(), 5);
                assert_eq!(item["status"], "Interrupted");
                assert_eq!(item["parts"][0]["text"], "before ");
                assert_eq!(item["parts"][1]["text"], "after");
                assert_eq!(item["parts"][2]["text"], "batched text");
                assert_eq!(item["parts"][3]["chunkIndex"], 1);
                assert_eq!(item["parts"][4]["partId"], "turn-3");
                assert_eq!(item["parts"][4]["chunkIndex"], 0);
                Ok::<_, anyhow::Error>(())
            }
            .await;
            runtime.shutdown().await?;
            handle.shutdown().await?;
            std::fs::remove_dir_all(root)?;
            result?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn project_creation_effect_replays_receipt_and_never_reexecutes_unknown_intent(
    ) -> Result<()> {
        fn make_writable(path: &std::path::Path) -> std::io::Result<()> {
            for entry in std::fs::read_dir(path)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    make_writable(&entry.path())?;
                } else {
                    let mut permissions = entry.metadata()?.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    permissions.set_readonly(false);
                    std::fs::set_permissions(entry.path(), permissions)?;
                }
            }
            Ok(())
        }

        for uncertain in [false, true] {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!("center-project-effect-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root)?;
            std::fs::write(
                root.join("adapters.json"),
                serde_json::to_vec(&json!({
                    "capabilities":[{"id":"fixture","adapter":{
                        "kind":"ACP","executable":"must-not-start.exe","args":[],
                        "approvedModelDestination":"Controlled test only"
                    }}]
                }))?,
            )?;
            let handle = super::super::transport::start_engine(root.clone()).await?;
            let runtime = Runtime::new(handle.clone(), root.clone())?;
            let result = async {
                runtime.register().await?;
                let target = root.join("new-project");
                let pending = runtime.request(Principal::Human, "project.configure", json!({
                    "name":"New project","root":target,"createDirectory":true,
                    "coordinatorCapabilityId":"fixture","workerCapabilityId":"fixture","checkCapabilityId":"native-check",
                    "limits":{"concurrency":1,"executionAttempts":2,"evaluationAttempts":2,
                        "coordinationTurns":2,"contextRounds":1,"executionSeconds":60,"coordinationSeconds":60}
                })).await;
                assert_eq!(pending.status, "pending", "{pending:?}");
                assert!(!target.exists());
                let effect = handle.effects().await?.into_iter()
                    .find(|effect| effect.method == "project.create").context("missing creation effect")?;
                if uncertain {
                    let fingerprint = artifacts::digest(&serde_json::to_vec(
                        &json!({"method":effect.method,"params":effect.params}),
                    )?);
                    std::fs::write(root.join("effects").join(format!("{}.intent", effect.id)), fingerprint)?;
                }
                runtime.execute(effect.clone()).await?;
                let operation = success(runtime.request(Principal::Human, "operation.get",
                    json!({"operationId":effect.id})).await)?["operation"].clone();
                if uncertain {
                    assert_eq!(operation["status"], "RepairRequired");
                    assert_eq!(operation["failure"]["code"], "OUTCOME_UNKNOWN");
                    assert!(!target.exists());
                } else {
                    assert_eq!(operation["status"], "Succeeded");
                    assert!(target.join(".git").is_dir());
                    let project = success(runtime.request(Principal::Human, "project.get",
                        json!({"projectId":operation["result"]["projectId"]})).await)?;
                    assert_eq!(project["root"], effect.params["project"]["root"]);
                    let head = workspace::git(&target, &["rev-parse", "HEAD"]).await?;
                    assert_eq!(project["initialCommitId"], head);
                    std::fs::write(target.join("keep.txt"), "Do not reinitialize")?;
                }
                runtime.execute(effect).await?;
                let replay = success(runtime.request(Principal::Human, "operation.get",
                    json!({"operationId":operation["id"]})).await)?["operation"].clone();
                assert_eq!(replay, operation);
                if !uncertain {
                    assert_eq!(std::fs::read_to_string(target.join("keep.txt"))?, "Do not reinitialize");
                }
                Ok::<_, anyhow::Error>(())
            }.await;
            runtime.shutdown().await?;
            handle.shutdown().await?;
            make_writable(&root)?;
            std::fs::remove_dir_all(&root)?;
            result?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn stopping_registered_conversation_before_startup_revokes_execution() -> Result<()> {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-stop-before-start-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("adapters.json"),
            serde_json::to_vec(&json!({
                "capabilities":[{"id":"fixture","adapter":{
                    "kind":"ACP","executable":"must-not-start.exe","args":[],
                    "approvedModelDestination":"Controlled test only"
                }}]
            }))?,
        )?;
        let handle = super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let result = async {
            runtime.register().await?;
            let conversation = Uuid::new_v4().to_string();
            let console = Uuid::new_v4().to_string();
            let submit = |message: &str| {
                json!({
                    "conversationId":conversation,"clientMessageId":Uuid::new_v4().to_string(),
                    "text":message,"attachments":[],
                    "context":{"scope":"Global","consoleSessionId":console,"contextVersion":1}
                })
            };
            success(
                runtime
                    .request(
                        Principal::Human,
                        "conversation.submit",
                        submit("Build a scene"),
                    )
                    .await,
            )?;
            let invoke = handle
                .effects()
                .await?
                .into_iter()
                .find(|effect| effect.method == "runtime.invoke")
                .context("missing invocation")?;
            let input = invoke.params["invocation"].clone();
            let id = text(&input, "id")?.to_owned();
            let (commands, receiver) = mpsc::channel(1);
            let invocation = Arc::new(Invocation {
                input,
                state: Mutex::new(InvocationState {
                    state: "NotStarted".into(),
                    settled: true,
                    ..Default::default()
                }),
                report_lock: Mutex::new(()),
                cancel: CancellationToken::new(),
                commands,
            });
            runtime
                .inner
                .invocations
                .lock()
                .await
                .insert(id.clone(), invocation.clone());
            handle
                .complete_effect(
                    &invoke.id,
                    Response::ok(
                        "",
                        json!({
                            "invocationId":id,"disposition":"Recorded"
                        }),
                    ),
                )
                .await?;
            success(
                runtime
                    .request(Principal::Human, "conversation.submit", submit("hi"))
                    .await,
            )?;
            let stop = handle
                .effects()
                .await?
                .into_iter()
                .find(|effect| effect.method == "runtime.stop")
                .context("missing scoped stop")?;
            runtime.execute(stop).await?;
            assert!(invocation.cancel.is_cancelled());
            assert!(runtime
                .run(invocation.clone(), receiver)
                .await
                .unwrap_err()
                .to_string()
                .contains("cancelled before execution"));
            assert!(invocation.state.lock().await.execution_identity.is_empty());
            let release = handle
                .effects()
                .await?
                .into_iter()
                .find(|effect| effect.method == "runtime.release")
                .context("missing release")?;
            runtime.execute(release).await?;
            assert!(handle.effects().await?.iter().any(|effect| {
                effect.method == "runtime.invoke" && effect.params["invocation"]["id"] != id
            }));
            Ok::<_, anyhow::Error>(())
        }
        .await;
        runtime.shutdown().await?;
        handle.shutdown().await?;
        std::fs::remove_dir_all(&root)?;
        result
    }

    #[tokio::test]
    async fn expired_work_with_absent_provider_reconciles_owned_job_before_resuming() -> Result<()>
    {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-owned-reconciliation-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root)?;
        std::fs::write(
            root.join("adapters.json"),
            serde_json::to_vec(&json!({
                "capabilities":[{"id":"fixture","adapter":{"kind":"ACP","executable":"must-not-start.exe","args":[],
                    "approvedModelDestination":"Controlled reconciliation fixture"}}]
            }))?,
        )?;
        let handle = super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        runtime.register().await?;
        let project_root = root.join("project");
        std::fs::create_dir_all(&project_root)?;
        let project_root = project_root.canonicalize()?;
        let guarded = |method: &str, params: Value, work: &Value| {
            let mut request = Request::new(method, params);
            request.command_id = Some(Uuid::new_v4().to_string());
            request.if_match = vec![super::super::wire::EntityRef {
                kind: "Work".into(),
                id: work["id"].as_str().unwrap().into(),
                version: work["version"].as_u64().unwrap(),
            }];
            request
        };
        let project = success(runtime.request(Principal::Human, "project.configure", json!({
            "name":"Expired recorded work","root":project_root,"coordinatorCapabilityId":"fixture",
            "workerCapabilityId":"fixture","checkCapabilityId":"native-check",
            "limits":{"concurrency":1,"executionAttempts":2,"evaluationAttempts":2,"coordinationTurns":3,
                "contextRounds":1,"executionSeconds":60,"coordinationSeconds":1}
        })).await)?;
        let draft = success(runtime.request(Principal::Human, "work.create_draft", json!({
            "executionMode":"LegacyTasks","projectId":project["projectId"],"goal":"Recover the same historical work","scope":["reports"],"exclusions":[],
            "criteria":[{"id":"report","description":"A readable report","evidenceRule":"artifact:report"}],
            "context":[],"delivery":{"kind":"Report"},"sourceMessageIds":[]
        })).await)?;
        let work_id = text(&draft, "workId")?.to_owned();
        let view = success(
            runtime
                .request(Principal::Human, "work.get", json!({"workId":work_id}))
                .await,
        )?;
        let grant = success(
            handle
                .request(
                    Principal::Human,
                    guarded(
                        "grant.preview",
                        json!({"workId":work_id,"specRevision":1,"policyRevision":1}),
                        &view["work"],
                    ),
                )
                .await,
        )?;
        let started = handle.request(Principal::Human, guarded("work.start",
            json!({"workId":work_id,"specRevision":1,"projectPolicyRevision":1,"grantProposalId":grant["grantProposalId"]}), &view["work"])).await;
        assert_eq!(started.status, "pending", "{started:?}");
        let workspace = handle
            .effects()
            .await?
            .into_iter()
            .find(|effect| effect.method == "workspace.provision")
            .context("missing approved workspace intent")?;
        handle
            .complete_effect(
                &workspace.id,
                Response::ok(
                    "",
                    json!({
                        "workspaceId":workspace.params["workspaceId"],"localRoot":project_root
                    }),
                ),
            )
            .await?;
        let invoke = handle
            .effects()
            .await?
            .into_iter()
            .find(|effect| effect.method == "runtime.invoke")
            .context("missing fixture invocation")?;
        let mut input = invoke.params["invocation"].clone();
        input["adapter"] = runtime.inner.adapters["fixture"].clone();
        let invocation_id = text(&input, "id")?.to_owned();
        let provider_configuration_digest =
            artifacts::digest(&serde_json::to_vec(&input["adapter"])?);
        let mut child = process::command("powershell.exe", &project_root)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "Start-Sleep -Seconds 30",
            ])
            .spawn()?;
        let identity = format!("acp:{}", child.id().context("fixture child PID missing")?);
        let job = Arc::new(process::ProcessJob::attach(&child)?);
        let (commands, _receiver) = mpsc::channel(1);
        let invocation = Arc::new(Invocation {
            input,
            state: Mutex::new(InvocationState {
                state: "Running".into(),
                turn: 1,
                execution_identity: identity.clone(),
                owned_job: Some(job.clone()),
                settled: false,
                primary_session: Some(PrimarySession {
                    work_id: work_id.clone(),
                    capability_id: "fixture".into(),
                    provider_configuration_digest: provider_configuration_digest.clone(),
                    provider_session_id: "fixture-session".into(),
                    cwd: project_root.clone(),
                }),
                ..Default::default()
            }),
            report_lock: Mutex::new(()),
            cancel: CancellationToken::new(),
            commands,
        });
        runtime
            .inner
            .invocations
            .lock()
            .await
            .insert(invocation_id.clone(), invocation.clone());
        handle
            .complete_effect(
                &invoke.id,
                Response::ok(
                    "",
                    json!({"invocationId":invocation_id,"disposition":"Recorded"}),
                ),
            )
            .await?;
        runtime
            .report(
                &invocation,
                "Started",
                json!({"adapterKind":"ACP","executionIdentity":identity,
                    "providerSessionId":"fixture-session","providerConfigurationDigest":provider_configuration_digest,
                    "sessionCwd":project_root,"sessionLoaded":false}),
            )
            .await?;
        job.terminate()?;
        job.settle().await?;
        child.wait().await?;
        assert!(
            child.try_wait()?.is_some(),
            "The recorded provider process must already be absent"
        );
        // The engine still has Running. The local sequence also simulates a lost
        // observation acknowledgment; only the authority may supply its committed cursor.
        invocation.state.lock().await.sequence = 0;
        if let Ok(remaining) = deadline(&invocation.input) {
            tokio::time::sleep(remaining + Duration::from_millis(10)).await;
        }
        let stale = success(
            runtime
                .request(Principal::Human, "work.get", json!({"workId":work_id}))
                .await,
        )?;
        assert_eq!(stale["continuation"]["state"], "NeedsRecovery");
        let continued = success(
            handle
                .request(
                    Principal::Human,
                    guarded("work.continue", json!({"workId":work_id}), &stale["work"]),
                )
                .await,
        )?;
        assert_eq!(continued["continuation"]["state"], "NeedsRecovery");
        assert_eq!(continued["continuation"]["canRestartSession"], false);
        let effects = handle.effects().await?;
        assert!(effects
            .iter()
            .all(|effect| effect.method != "runtime.invoke"));
        let stop = effects
            .into_iter()
            .find(|effect| effect.method == "runtime.stop")
            .context("missing exact scoped reconciliation")?;
        assert_eq!(stop.params["reconciliation"]["executionIdentity"], identity);
        assert_eq!(stop.params["reconciliation"]["lastSequence"], 1);
        let reconciliation = stop.params.clone();
        runtime.execute(stop).await?;
        assert!(invocation.state.lock().await.settled);
        assert_eq!(invocation.state.lock().await.sequence, 2);
        let saved: Value =
            serde_json::from_slice(&std::fs::read(runtime.ledger(&invocation_id)?)?)?;
        assert_eq!(saved["state"]["settlementProof"], identity);
        assert!(saved["state"]["terminalObservation"].is_null());
        let restarted = Runtime::new(handle.clone(), root.clone())?;
        assert!(
            restarted
                .restore_settled_invocation(&reconciliation)
                .await?
                .state
                .lock()
                .await
                .settled
        );
        let release = handle
            .effects()
            .await?
            .into_iter()
            .find(|effect| effect.method == "runtime.release")
            .context("proven reconciliation must release its binding")?;
        runtime.execute(release).await?;
        assert!(invocation.state.lock().await.released);
        let next = handle
            .effects()
            .await?
            .into_iter()
            .find(|effect| effect.method == "runtime.invoke")
            .context("proven release should admit same-work continuation")?;
        assert_eq!(next.params["invocation"]["sessionReuseRef"], invocation_id);
        assert_eq!(
            next.params["invocation"]["coordinationInput"]["scope"]["workId"],
            work_id
        );
        let mut resumed_input = next.params["invocation"].clone();
        resumed_input["adapter"] = runtime.inner.adapters["fixture"].clone();
        assert_eq!(
            runtime
                .saved_primary_session(&invocation_id, &resumed_input)?
                .provider_session_id,
            "fixture-session"
        );
        runtime.shutdown().await?;
        handle.shutdown().await?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn restored_reconciliation_requires_durable_tree_settlement_not_pid_absence() -> Result<()>
    {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-ledger-reconciliation-{}", Uuid::new_v4()));
        let handle = super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let invocation_id = Uuid::new_v4().to_string();
        let runtime_id = Uuid::new_v4().to_string();
        let params = json!({"invocationId":invocation_id,"operationId":Uuid::new_v4().to_string(),
            "reconciliation":{"runtimeId":runtime_id,"bindingGeneration":1,"lastSequence":131,
                "executionIdentity":"acp:28180","stopOperationIds":[]}});
        let mut state = InvocationState {
            state: "Running".into(),
            execution_identity: "acp:28180".into(),
            sequence: 130,
            settled: false,
            ..Default::default()
        };
        let save = |state: &InvocationState| -> Result<()> {
            std::fs::write(
                runtime.ledger(&invocation_id)?,
                serde_json::to_vec(&json!({"state":state}))?,
            )?;
            Ok(())
        };
        save(&state)?;
        assert!(runtime.restore_settled_invocation(&params).await.is_err());
        state.settled = true;
        save(&state)?;
        assert!(runtime.restore_settled_invocation(&params).await.is_err());
        state.terminal_observation = Some(json!({"sequence":130,"data":{"quiescent":true}}));
        save(&state)?;
        let mut mismatched = params.clone();
        mismatched["reconciliation"]["executionIdentity"] = json!("acp:another-execution");
        assert!(runtime
            .restore_settled_invocation(&mismatched)
            .await
            .is_err());
        let restored = runtime.restore_settled_invocation(&params).await?;
        assert!(restored.state.lock().await.settled);
        assert!(restored.cancel.is_cancelled());
        assert!(
            restored.state.lock().await.primary_session.is_none(),
            "legacy settlement does not invent a resumable primary session"
        );
        handle.shutdown().await?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    #[ignore = "launched by the parent test to verify the real process working directory"]
    fn coordination_directory_child() {
        let expected = PathBuf::from(std::env::var_os("WTA_TEST_COORDINATION_CWD").unwrap());
        let actual = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(actual, expected);
        std::fs::write(actual.join("cwd-proof.txt"), "exact coordination directory").unwrap();
    }

    #[tokio::test]
    async fn coordination_directory_launches_at_and_beyond_the_windows_path_boundary() {
        use std::os::windows::ffi::OsStrExt;

        let temporary = std::env::temp_dir().join(format!("wta-cwd-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&temporary).await.unwrap();
        let executable = std::env::current_exe().unwrap();
        for length in [180usize, 260, 300] {
            let invocation_id = Uuid::new_v4().to_string();
            let prefix = temporary.join("p");
            let measured = prefix.join("coordination").join(&invocation_id);
            let padding = length
                .checked_sub(measured.as_os_str().encode_wide().count())
                .expect("temporary root leaves room for the exact path boundary")
                + 1;
            let root = temporary.join("p".repeat(padding));
            let plain = root.join("coordination").join(&invocation_id);
            assert_eq!(plain.as_os_str().encode_wide().count(), length);
            let cwd = Runtime::coordination_directory(&root, &invocation_id)
                .await
                .unwrap();
            assert_eq!(cwd, plain.canonicalize().unwrap());
            let launch_cwd = process::launch_directory(&cwd).await.unwrap();
            let output = tokio::time::timeout(
                Duration::from_secs(15),
                process::command(executable.to_str().unwrap(), &launch_cwd)
                    .args([
                        "--exact",
                        "agent_center::runtime::tests::coordination_directory_child",
                        "--ignored",
                        "--nocapture",
                    ])
                    .env("WTA_TEST_COORDINATION_CWD", &cwd)
                    .output(),
            )
            .await
            .unwrap()
            .unwrap();
            assert!(
                output.status.success(),
                "length {length}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                tokio::fs::read_to_string(cwd.join("cwd-proof.txt"))
                    .await
                    .unwrap(),
                "exact coordination directory"
            );
        }
        let mut unshortenable = temporary.join("already83");
        for _ in 0..35 {
            unshortenable.push("abcdefgh");
        }
        tokio::fs::create_dir_all(&unshortenable).await.unwrap();
        assert!(process::launch_directory(&unshortenable)
            .await
            .unwrap_err()
            .to_string()
            .contains("no usable existing short name"));
        let root = temporary.join("unavailable");
        tokio::fs::create_dir_all(root.join("coordination"))
            .await
            .unwrap();
        let invocation_id = Uuid::new_v4().to_string();
        tokio::fs::write(
            root.join("coordination").join(&invocation_id),
            "not a directory",
        )
        .await
        .unwrap();
        assert!(Runtime::coordination_directory(&root, &invocation_id)
            .await
            .unwrap_err()
            .to_string()
            .contains("create coordination working directory"));
        tokio::fs::remove_dir_all(temporary).await.unwrap();
    }

    #[test]
    fn invocation_dedup_preserves_identity_and_rejects_changed_contract() {
        let original = json!({"id":Uuid::new_v4().to_string(),"dispatch":{"id":"dispatch","contractDigest":"sha256:original"}});
        let response = duplicate_receipt(&original, &original);
        assert_eq!(response.status, "ok");
        assert_eq!(response.data.unwrap()["disposition"], "AlreadyRecorded");
        let mut changed = original.clone();
        changed["dispatch"]["contractDigest"] = json!("sha256:changed");
        let response = duplicate_receipt(&original, &changed);
        assert_eq!(response.failure.unwrap().code, "COMMAND_ID_REUSED");
    }

    #[tokio::test]
    async fn recovered_coordinator_preserves_provenance_without_fabricating_settlement(
    ) -> Result<()> {
        for resumed in [false, true] {
            let root = std::env::current_dir()?
                .join("target")
                .join(format!("center-recovered-session-{}", Uuid::new_v4()));
            std::fs::create_dir_all(&root)?;
            let adapter = json!({"kind":"ACP","executable":"fixture-provider","approvedModelDestination":"Controlled test only"});
            std::fs::write(
                root.join("adapters.json"),
                serde_json::to_vec(&json!({
                    "capabilities":[{"id":"fixture","adapter":adapter}]
                }))?,
            )?;
            let handle = super::super::transport::start_engine(root.clone()).await?;
            let runtime = Runtime::new(handle.clone(), root.clone())?;
            let id = Uuid::new_v4().to_string();
            let work = Uuid::new_v4().to_string();
            let mut original = json!({"id":id,"capabilityId":"fixture","subject":{"kind":"Coordination","id":Uuid::new_v4().to_string()},
            "coordinationInput":{"scope":{"workId":work},"snapshot":{"work":{"workspaceId":"workspace"}}}});
            if resumed {
                original["sessionReuseRef"] = json!(Uuid::new_v4().to_string());
            }
            let mut input = original.clone();
            input["adapter"] = adapter.clone();
            let session = PrimarySession {
                work_id: work.clone(),
                capability_id: "fixture".into(),
                provider_configuration_digest: artifacts::digest(&serde_json::to_vec(&adapter)?),
                provider_session_id: "original-session".into(),
                cwd: root.canonicalize()?,
            };
            if resumed {
                input["resumeSession"] = serde_json::to_value(&session)?;
            }
            let state = InvocationState {
                state: "Running".into(),
                execution_identity: "old-coordinator".into(),
                primary_session: Some(session),
                ..Default::default()
            };
            let ledger = json!({"inputDigest":artifacts::digest(&serde_json::to_vec(&input)?),"state":state});
            let path = runtime.ledger(&id)?;
            let bytes = serde_json::to_vec(&ledger)?;
            std::fs::write(&path, &bytes)?;
            let params = json!({"invocation":original,"workId":work,"workspaceId":"workspace",
            "providerSessionId":"original-session","executionIdentity":"old-coordinator"});
            let recovered = runtime.recover_coordinator_session(&params).await?;
            assert_eq!(recovered.provider_session_id, "original-session");
            assert_eq!(
                std::fs::read(&path)?,
                bytes,
                "Recovery must not rewrite process settlement evidence"
            );
            let restarted = Runtime::new(handle.clone(), root.clone())?;
            assert_eq!(
                restarted
                    .saved_primary_session(&id, &input)?
                    .provider_session_id,
                "original-session"
            );
            assert_eq!(
                restarted
                    .recover_coordinator_session(&params)
                    .await?
                    .provider_session_id,
                "original-session"
            );
            for pointer in [
                "/workId",
                "/executionIdentity",
                "/invocation/subject/kind",
                "/invocation/capabilityId",
                "/providerSessionId",
            ] {
                let mut changed = params.clone();
                *changed.pointer_mut(pointer).unwrap() = json!("different");
                assert!(
                    runtime.recover_coordinator_session(&changed).await.is_err(),
                    "{pointer}"
                );
            }
            let mut changed = input.clone();
            changed["adapter"]["executable"] = json!("different-provider");
            assert!(runtime.saved_primary_session(&id, &changed).is_err());
            handle.shutdown().await?;
            std::fs::remove_dir_all(root)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn saved_primary_session_is_durable_and_pins_work_provider_and_settlement() -> Result<()>
    {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-session-ledger-{}", Uuid::new_v4()));
        let handle = super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let source = Uuid::new_v4().to_string();
        let work = Uuid::new_v4().to_string();
        let adapter =
            json!({"kind":"ACP","executable":"test-provider","args":[],"model":"approved-model"});
        let session = PrimarySession {
            work_id: work.clone(),
            capability_id: "test-provider".into(),
            provider_configuration_digest: artifacts::digest(&serde_json::to_vec(&adapter)?),
            provider_session_id: "saved-main-session".into(),
            cwd: root.clone(),
        };
        let mut state = InvocationState {
            released: true,
            settled: true,
            primary_session: Some(session),
            ..Default::default()
        };
        let input = json!({"capabilityId":"test-provider","adapter":adapter,
            "coordinationInput":{"scope":{"workId":work}}});
        let save = |state: &InvocationState| -> Result<()> {
            std::fs::write(
                runtime.ledger(&source)?,
                serde_json::to_vec(&json!({"state":state}))?,
            )?;
            Ok(())
        };
        save(&state)?;
        assert_eq!(
            runtime
                .saved_primary_session(&source, &input)?
                .provider_session_id,
            "saved-main-session"
        );
        let restarted = Runtime::new(handle.clone(), root.clone())?;
        assert_eq!(
            restarted
                .saved_primary_session(&source, &input)?
                .provider_session_id,
            "saved-main-session"
        );
        for pointer in [
            "/capabilityId",
            "/adapter/model",
            "/coordinationInput/scope/workId",
        ] {
            let mut wrong = input.clone();
            *wrong.pointer_mut(pointer).unwrap() = json!("different");
            assert!(runtime
                .saved_primary_session(&source, &wrong)
                .unwrap_err()
                .to_string()
                .contains("does not match"));
        }
        state
            .primary_session
            .as_mut()
            .unwrap()
            .provider_session_id
            .clear();
        save(&state)?;
        assert!(runtime
            .saved_primary_session(&source, &input)
            .unwrap_err()
            .to_string()
            .contains("malformed"));
        state.primary_session.as_mut().unwrap().provider_session_id = "saved-main-session".into();
        state.primary_session.as_mut().unwrap().cwd = PathBuf::from("relative-cwd");
        save(&state)?;
        assert!(runtime
            .saved_primary_session(&source, &input)
            .unwrap_err()
            .to_string()
            .contains("malformed"));
        state.primary_session.as_mut().unwrap().cwd = root.clone();
        state.released = false;
        save(&state)?;
        assert!(runtime
            .saved_primary_session(&source, &input)
            .unwrap_err()
            .to_string()
            .contains("not proven settled"));
        state.released = true;
        state.settled = false;
        save(&state)?;
        assert!(runtime.saved_primary_session(&source, &input).is_err());
        state.settled = true;
        state.primary_session = None;
        save(&state)?;
        assert!(runtime
            .saved_primary_session(&source, &input)
            .unwrap_err()
            .to_string()
            .contains("association is missing"));
        assert!(runtime
            .saved_primary_session("..\\foreign", &input)
            .is_err());
        assert!(runtime
            .saved_primary_session(&Uuid::new_v4().to_string(), &input)
            .is_err());
        std::fs::write(runtime.ledger(&source)?, b"{malformed ledger")?;
        assert!(runtime.saved_primary_session(&source, &input).is_err());
        handle.shutdown().await?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[tokio::test]
    async fn resumed_coordination_rejects_missing_or_retargeted_cwd_without_starting_provider(
    ) -> Result<()> {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("center-session-cwd-{}", Uuid::new_v4()));
        let handle = super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        tokio::fs::create_dir_all(root.join("coordination")).await?;
        for cwd in [
            root.clone(),
            root.join("coordination").join("missing-history"),
        ] {
            let (commands, controls) = mpsc::channel(1);
            let invocation = Arc::new(Invocation {
                input: json!({"id":Uuid::new_v4().to_string(),"limits":{"deadlineUtc":"2999-01-01T00:00:00Z"},
                    "coordinationInput":{"scope":{"workId":"fixed-work"}},
                    "resumeSession":{"workId":"fixed-work","capabilityId":"fixed-provider",
                        "providerConfigurationDigest":"fixed-digest","providerSessionId":"saved-session","cwd":cwd}}),
                state: Mutex::new(InvocationState {
                    settled: true,
                    ..Default::default()
                }),
                report_lock: Mutex::new(()),
                cancel: CancellationToken::new(),
                commands,
            });
            assert!(format!(
                "{:#}",
                runtime.run(invocation.clone(), controls).await.unwrap_err()
            )
            .contains("SESSION_RESUME_UNAVAILABLE"));
            assert!(invocation.state.lock().await.execution_identity.is_empty());
            assert!(invocation.state.lock().await.settled);
        }
        handle.shutdown().await?;
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn expired_invocation_cannot_start_a_provider() {
        assert!(deadline(&json!({"limits":{"deadlineUtc":"2020-01-01T00:00:00Z"}})).is_err());
        assert!(deadline(&json!({"limits":{}})).is_err());
    }

    #[test]
    fn acp_configuration_requires_explicit_destination_approval() {
        let mut configuration = json!({"capabilities":[{"id":"approved-agent","adapter":{
            "kind":"ACP","executable":"controlled-agent.exe","args":[],"model":"approved-model",
            "environment":{"PROVIDER_BASE_URL":"https://example.invalid"}
        }}]});
        assert!(
            validate_adapter_configuration(&serde_json::to_vec(&configuration).unwrap()).is_err()
        );
        configuration["capabilities"][0]["adapter"]["approvedModelDestination"] = json!(" ");
        assert!(
            validate_adapter_configuration(&serde_json::to_vec(&configuration).unwrap()).is_err()
        );
        configuration["capabilities"][0]["adapter"]["approvedModelDestination"] =
            json!("Local deterministic fixture; no model service");
        validate_adapter_configuration(&serde_json::to_vec(&configuration).unwrap()).unwrap();
    }

    #[test]
    fn a_single_approved_adapter_bootstraps_chat_without_a_project() {
        let config = json!({"capabilities":[{"id":"approved","adapter":{
            "kind":"ACP","executable":"controlled-agent.exe",
            "approvedModelDestination":"Local scripted adapter"
        }}]});
        let bytes = serde_json::to_vec(&config).unwrap();
        let adapters = read_adapters(&bytes).unwrap();
        let policy = read_conversation_policy(&bytes, &adapters)
            .unwrap()
            .unwrap();
        assert_eq!(policy["capabilityId"], "approved");
        assert_eq!(policy["approvedModelDestination"], "Local scripted adapter");
        assert_eq!(policy["limits"]["coordinationTurns"], 64);
        assert_eq!(policy["limits"]["coordinationSeconds"], 120);
        assert!(policy.get("projectId").is_none());
        assert!(policy.get("root").is_none());
    }

    #[test]
    fn multiple_approved_adapters_require_an_explicit_conversation_choice() {
        let mut config = json!({"capabilities":[
            {"id":"first","adapter":{"kind":"ACP","executable":"first.exe","approvedModelDestination":"First approved destination"}},
            {"id":"second","adapter":{"kind":"ACP","executable":"second.exe","approvedModelDestination":"Second approved destination"}}
        ]});
        let bytes = serde_json::to_vec(&config).unwrap();
        let adapters = read_adapters(&bytes).unwrap();
        assert!(read_conversation_policy(&bytes, &adapters)
            .unwrap()
            .is_none());
        config["conversationCapabilityId"] = json!("second");
        let policy = read_conversation_policy(&serde_json::to_vec(&config).unwrap(), &adapters)
            .unwrap()
            .unwrap();
        assert_eq!(policy["capabilityId"], "second");
        assert_eq!(
            policy["approvedModelDestination"],
            "Second approved destination"
        );
        config["conversationCapabilityId"] = json!("not-approved");
        assert!(
            read_conversation_policy(&serde_json::to_vec(&config).unwrap(), &adapters).is_err()
        );
    }

    #[test]
    fn malformed_conversation_limits_are_not_silently_ignored() {
        let config = json!({"capabilities":[],"conversationLimits":{"coordinationTurns":0}});
        assert!(validate_adapter_configuration(&serde_json::to_vec(&config).unwrap()).is_err());
        let config = json!({"capabilities":[{"id":"approved","adapter":{
            "kind":"ACP","executable":"controlled-agent.exe","approvedModelDestination":"Approved"
        }}],"conversationLimits":{"coordinationTurns":0}});
        assert!(validate_adapter_configuration(&serde_json::to_vec(&config).unwrap()).is_err());
    }
}
