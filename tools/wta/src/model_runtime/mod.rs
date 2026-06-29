//! Model-runtime abstraction — the per-backend lifecycle + discovery seam.
//!
//! Where [`crate::llm_provider`] models a single *generic* backend config and
//! [`crate::agent`] owns how an agent CLI *expresses* that config, this module
//! answers the operational questions for each concrete local backend:
//!
//! * **discovery** — which models is this runtime serving right now?
//! * **availability** — is the backend installed / reachable at all?
//! * **lifecycle** — start the daemon if it's down (Foundry/Ollama won't be
//!   started by the agent CLI; copilot's local support is limited, so we fill
//!   the gap).
//! * **routing** — the generic [`LlmProviderConfig`] to point an agent at a
//!   given model on this runtime.
//!
//! A [`ModelRuntime`] serves *many* models; each [`crate::agent::ModelEntry`]
//! remembers which runtime backs it (via its [`ModelKind`] tag today, an
//! explicit [`RuntimeId`] as the abstraction grows). The `/model` picker simply
//! aggregates [`ModelRuntime::list_models`] across the registry plus the cloud
//! catalog — the client never branches on local-vs-cloud, that complexity lives
//! here and in the agent.
//!
//! ## Status
//!
//! Step 1 introduces the trait + registry and migrates the existing env-sourced
//! local-discovery path behind [`FoundryRuntime`]. [`aggregate_models`]
//! reproduces the previous `copilot::resolve_models` logic; the registry now
//! auto-probes both Foundry Local and Ollama so the picker can surface running
//! local model providers without extra config.

use std::collections::HashSet;
use std::future::Future;

use foundry_local_sdk::{FoundryLocalConfig, FoundryLocalManager};

use crate::agent::{ModelCatalog, ModelEntry, ModelKind};
use crate::llm_provider::{discover_local_models, LlmProviderConfig};

/// Stable identifier for an inference backend. Persisted in the provider
/// selection and used to look a runtime back up on respawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuntimeId {
    /// The agent's hosted/cloud backend (the ACP-advertised catalog).
    Cloud,
    /// Microsoft Foundry Local (OpenAI-compatible, dynamic localhost port).
    Foundry,
    /// Ollama (OpenAI-compatible at `http://localhost:11434/v1`).
    Ollama,
}

impl RuntimeId {
    /// The display tag for this runtime's models.
    pub fn kind(self) -> ModelKind {
        match self {
            RuntimeId::Cloud => ModelKind::Cloud,
            RuntimeId::Foundry | RuntimeId::Ollama => ModelKind::Local,
        }
    }
}

/// A concrete inference backend that can serve models to an agent CLI.
///
/// One implementation per backend. A runtime serves many models; methods are
/// `&self` and return owned data so a registry can hand out trait objects.
pub trait ModelRuntime {
    /// Stable backend identifier.
    fn id(&self) -> RuntimeId;

    /// The display tag for this runtime's models (`Cloud`/`Local`).
    fn kind(&self) -> ModelKind {
        self.id().kind()
    }

    /// Human-facing backend name (e.g. `"Ollama"`, `"Foundry Local"`).
    fn display_name(&self) -> &'static str;

    /// Whether this backend is configured/installed and worth probing. Cloud is
    /// always available; a local runtime is available when its endpoint is
    /// configured (or its daemon is reachable).
    fn is_available(&self) -> bool;

    /// The models this runtime is serving right now, by id. Empty when the
    /// backend is down or advertises nothing (the probe degrades gracefully).
    fn list_models(&self) -> Vec<String> {
        Vec::new()
    }

    /// A short, per-runtime description surfaced under each model in the picker
    /// (e.g. the endpoint URL). `None` for runtimes with nothing to add.
    fn description(&self) -> Option<String> {
        None
    }

    /// Ensure the backing service is running, starting it if necessary. No-op
    /// for cloud and for already-running local daemons. Wired up in a later
    /// step; the default is a no-op so the trait stays usable now.
    fn ensure_running(&self) -> std::io::Result<()> {
        Ok(())
    }

    /// The generic provider config to point an agent CLI at `model` on this
    /// runtime, or `None` when no local routing applies (cloud, or an
    /// unconfigured local runtime). The agent translates this into its own env
    /// contract via [`crate::agent::Agent::byok_env`].
    fn provider_config(&self, _model: &str) -> Option<LlmProviderConfig> {
        None
    }
}

/// The agent's hosted/cloud backend. Carries no local routing — its models come
/// from the ACP catalog, and switching to it means *stripping* the BYOK env.
pub struct CloudRuntime;

impl ModelRuntime for CloudRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::Cloud
    }
    fn display_name(&self) -> &'static str {
        "Cloud"
    }
    fn is_available(&self) -> bool {
        true
    }
}

/// Microsoft Foundry Local, discovered through the Rust SDK when cached local
/// models are available. This keeps the provider zero-config in the common
/// local case while still honoring an explicit provider URL when the user sets
/// one.
pub struct FoundryRuntime {
    cfg: Option<LlmProviderConfig>,
    cached_models: Vec<String>,
}

impl FoundryRuntime {
    /// Build from the ambient BYOK environment, if one is configured.
    pub fn from_env() -> Self {
        Self {
            cfg: foundry_cfg_from_env(),
            cached_models: Vec::new(),
        }
    }

    /// Probe the Foundry Local SDK for cached models, falling back to the
    /// ambient env when no SDK-managed models are present.
    pub fn detect() -> Self {
        let cached_models = foundry_cached_model_ids();
        if !cached_models.is_empty() {
            return Self {
                cfg: None,
                cached_models,
            };
        }

        Self {
            cfg: foundry_cfg_from_env(),
            cached_models,
        }
    }
}

impl ModelRuntime for FoundryRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::Foundry
    }
    fn display_name(&self) -> &'static str {
        "Foundry Local"
    }
    fn is_available(&self) -> bool {
        self.cfg.is_some() || !self.cached_models.is_empty()
    }
    fn list_models(&self) -> Vec<String> {
        if !self.cached_models.is_empty() {
            return self.cached_models.clone();
        }

        match self.cfg.as_ref().and_then(|cfg| cfg.base_url.as_deref()) {
            Some(url) if !url.is_empty() => discover_local_models(url)
                .into_iter()
                .map(|m| m.id)
                .collect(),
            _ => Vec::new(),
        }
    }
    fn description(&self) -> Option<String> {
        if let Some(url) = self
            .cfg
            .as_ref()
            .and_then(|cfg| cfg.base_url.as_deref())
            .filter(|s| !s.is_empty())
        {
            return Some(format!("Local provider · {url}"));
        }

        if !self.cached_models.is_empty() {
            return Some("Foundry Local".to_string());
        }

        None
    }
    fn provider_config(&self, model: &str) -> Option<LlmProviderConfig> {
        if let Some(mut cfg) = self.cfg.as_ref().cloned() {
            cfg.model = Some(model.to_string());
            return Some(cfg);
        }

        if self.cached_models.is_empty() {
            return None;
        }

        let base_url = foundry_service_base_url()?;
        Some(LlmProviderConfig {
            base_url: Some(base_url),
            api_key: None,
            provider_type: Some("openai".to_string()),
            model: Some(model.to_string()),
            offline: false,
        })
    }
    fn ensure_running(&self) -> std::io::Result<()> {
        if self.cfg.is_some() || self.cached_models.is_empty() {
            return Ok(());
        }

        foundry_service_base_url().map(|_| ()).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                "Foundry Local service did not become ready",
            )
        })
    }
}

/// Ollama, the widely-adopted local model runner. Auto-probed at its fixed
/// default endpoint `http://127.0.0.1:11434` — no env configuration needed, so
/// a running Ollama daemon surfaces its models in the picker automatically
/// (the gap copilot's limited local support leaves open). Ollama exposes an
/// OpenAI-compatible API, so the same generic discovery + BYOK env path works.
pub struct OllamaRuntime;

/// Ollama's fixed localhost endpoint (OpenAI-compatible surface under `/v1`).
const OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434/v1";
/// Cheap reachability probe budget for the daemon TCP connect.
const OLLAMA_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(300);

fn foundry_cfg_from_env() -> Option<LlmProviderConfig> {
    let cfg = LlmProviderConfig::from_env();
    if cfg.base_url.as_deref().is_some_and(|s| !s.is_empty()) {
        Some(cfg)
    } else {
        None
    }
}

fn foundry_cached_model_ids() -> Vec<String> {
    let Some(manager) = foundry_manager() else {
        return Vec::new();
    };

    let result = foundry_block_on_result(async {
        let models = manager.catalog().get_cached_models().await?;
        Ok::<_, foundry_local_sdk::FoundryLocalError>(
            models
                .into_iter()
                .map(|model| model.id().to_string())
                .collect::<Vec<_>>(),
        )
    });

    match result {
        Some(ref ids) if !ids.is_empty() => {
            tracing::info!(
                target: "model_runtime",
                count = ids.len(),
                ids = ?ids,
                "Foundry Local: cached models found"
            );
        }
        Some(_) => {
            tracing::info!(
                target: "model_runtime",
                "Foundry Local: SDK initialized but no cached models found (use `foundry model download <model>` to cache one)"
            );
        }
        None => {
            tracing::warn!(
                target: "model_runtime",
                "Foundry Local: get_cached_models() failed"
            );
        }
    }

    result.unwrap_or_default()
}

fn foundry_manager() -> Option<&'static FoundryLocalManager> {
    match FoundryLocalManager::create(FoundryLocalConfig::new("intelligent_terminal")) {
        Ok(manager) => {
            tracing::info!(target: "model_runtime", "Foundry Local: SDK manager initialized successfully");
            Some(manager)
        }
        Err(e) => {
            tracing::warn!(
                target: "model_runtime",
                error = %e,
                "Foundry Local: SDK manager init failed — Foundry Local may not be installed"
            );
            None
        }
    }
}

fn foundry_block_on_result<T, E>(
    future: impl Future<Output = std::result::Result<T, E>>,
) -> Option<T> {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(future).ok())
    } else {
        tokio::runtime::Runtime::new().ok()?.block_on(future).ok()
    }
}

fn foundry_service_base_url() -> Option<String> {
    let manager = foundry_manager()?;
    if manager
        .urls()
        .ok()
        .filter(|urls| !urls.is_empty())
        .is_none()
    {
        foundry_block_on_result(manager.start_web_service())?;
    }
    foundry_base_url_from_urls(&manager.urls().ok()?)
}

fn foundry_base_url_from_urls(urls: &[String]) -> Option<String> {
    urls.first()
        .map(|url| format!("{}{}", url.trim_end_matches('/'), "/v1"))
}

impl ModelRuntime for OllamaRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::Ollama
    }
    fn display_name(&self) -> &'static str {
        "Ollama"
    }
    fn is_available(&self) -> bool {
        // A cheap TCP connect to the daemon — fails fast when Ollama isn't
        // running, so we skip the (slower) model GET entirely in that case.
        port_open("127.0.0.1", 11434, OLLAMA_PROBE_TIMEOUT)
    }
    fn list_models(&self) -> Vec<String> {
        discover_local_models(OLLAMA_BASE_URL)
            .into_iter()
            .map(|m| m.id)
            .collect()
    }
    fn description(&self) -> Option<String> {
        Some("Ollama · http://localhost:11434".to_string())
    }
    fn provider_config(&self, model: &str) -> Option<LlmProviderConfig> {
        Some(LlmProviderConfig {
            base_url: Some(OLLAMA_BASE_URL.to_string()),
            // Ollama ignores the key but the OpenAI client contract wants one.
            api_key: Some("ollama".to_string()),
            provider_type: Some("openai".to_string()),
            model: Some(model.to_string()),
            offline: false,
        })
    }
    /// Start the Ollama daemon if it isn't already listening. This fills the gap
    /// copilot's limited local support leaves open: copilot won't launch the
    /// backend, so before respawning the agent CLI against an Ollama model we
    /// make sure `ollama serve` is up. No-op when the daemon already answers on
    /// the fixed port; otherwise spawns it detached and waits briefly for it to
    /// accept connections. A missing `ollama` binary is reported as an error so
    /// the caller can surface "Ollama not installed".
    fn ensure_running(&self) -> std::io::Result<()> {
        if port_open("127.0.0.1", 11434, OLLAMA_PROBE_TIMEOUT) {
            return Ok(());
        }
        // Launch the daemon detached — it backgrounds itself and must outlive
        // this spawn helper.
        std::process::Command::new("ollama")
            .arg("serve")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to start `ollama serve` (is Ollama installed?): {e}"),
                )
            })?;
        // Poll until the daemon accepts connections, capped so a wedged start
        // can't hang the agent respawn indefinitely.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if port_open("127.0.0.1", 11434, OLLAMA_PROBE_TIMEOUT) {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Ollama daemon did not become ready within 10s",
        ))
    }
}

/// Best-effort TCP reachability check used by local runtimes to detect a
/// running daemon cheaply before issuing a model-list request.
fn port_open(host: &str, port: u16, timeout: std::time::Duration) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = (host, port).to_socket_addrs() else {
        return false;
    };
    addrs.any(|addr| std::net::TcpStream::connect_timeout(&addr, timeout).is_ok())
}

/// The local runtimes to aggregate, in display order. Ollama leads (fixed,
/// zero-config endpoint) followed by the SDK-backed Foundry runtime with an
/// env fallback for explicit OpenAI-compatible endpoints.
pub fn local_runtimes() -> Vec<Box<dyn ModelRuntime>> {
    vec![Box::new(OllamaRuntime), Box::new(FoundryRuntime::detect())]
}

/// Resolve the concrete [`ModelRuntime`] for a [`RuntimeId`]. Local runtimes are
/// sourced the same way [`local_runtimes`] builds them (Foundry via SDK when
/// available, otherwise the ambient env fallback). Used by the spawner to
/// translate a persisted [`RuntimeId`] selection into the provider env for the
/// next agent-CLI spawn.
pub fn runtime_for_id(id: RuntimeId) -> Box<dyn ModelRuntime> {
    match id {
        RuntimeId::Cloud => Box::new(CloudRuntime),
        RuntimeId::Foundry => Box::new(FoundryRuntime::detect()),
        RuntimeId::Ollama => Box::new(OllamaRuntime),
    }
}

/// The provider config to route `model` on runtime `id`, or `None` for cloud /
/// an unconfigured local runtime. Thin convenience over [`runtime_for_id`] +
/// [`ModelRuntime::provider_config`].
pub fn runtime_provider_config(id: RuntimeId, model: &str) -> Option<LlmProviderConfig> {
    match id {
        RuntimeId::Cloud => None,
        RuntimeId::Foundry => FoundryRuntime::detect()
            .provider_config(model)
            .or_else(|| FoundryRuntime::from_env().provider_config(model)),
        RuntimeId::Ollama => runtime_for_id(id).provider_config(model),
    }
}

/// Which local runtime currently serves `model_id`, by probing each available
/// local runtime's model list. `None` when no available local runtime lists it
/// (a cloud model, or a local daemon that's momentarily unreachable). Used at
/// `/model` pick time to record the concrete [`RuntimeId`] in the persisted
/// [`crate::llm_provider::ProviderSelection`].
pub fn runtime_for_model(model_id: &str) -> Option<RuntimeId> {
    for rt in local_runtimes() {
        if rt.is_available() && rt.list_models().iter().any(|m| m == model_id) {
            return Some(rt.id());
        }
    }
    None
}

/// Aggregate the cloud catalog with every available local runtime's models into
/// the tagged picker list.
///
/// `pinned` is the model the agent is *actually* pinned to out-of-band (copilot
/// BYOK's `COPILOT_MODEL`); when present it leads the list, is marked current,
/// and forces `switchable = false` (the env-pinned catalog can't be switched
/// live). When `pinned` is `None` the cloud catalog is authoritative and passes
/// through unchanged — matching the previous `resolve_models` behavior.
pub fn aggregate_models(cloud: ModelCatalog, pinned: Option<String>) -> ModelCatalog {
    aggregate_with(cloud, pinned, local_runtimes())
}

/// [`aggregate_models`] with an explicit runtime set — the testable core, so
/// unit tests don't depend on which local daemons happen to be running.
///
/// Two cases:
/// * **A model is pinned** (`pinned = Some`, copilot BYOK's `COPILOT_MODEL`):
///   the agent is env-pinned to that local model. It leads the list, is marked
///   current, and the catalog is `switchable = false` (can't switch live).
/// * **No pin** (`pinned = None`): the agent started on its cloud backend, but
///   we still surface any running local runtime's models (zero-config Ollama
///   discovery). The cloud `current_id`/`switchable` are preserved; selecting a
///   local model is a cross-runtime switch the caller handles by respawning.
///
/// When there is neither a pin nor any available local model, the cloud catalog
/// passes through unchanged.
pub fn aggregate_with(
    cloud: ModelCatalog,
    pinned: Option<String>,
    runtimes: Vec<Box<dyn ModelRuntime>>,
) -> ModelCatalog {
    let mut models: Vec<ModelEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for rt in &runtimes {
        if !rt.is_available() {
            continue;
        }
        let desc = rt.description();
        for id in rt.list_models() {
            if seen.insert(id.clone()) {
                models.push(ModelEntry {
                    name: id.clone(),
                    id,
                    description: desc.clone(),
                    kind: ModelKind::Local,
                });
            }
        }
    }

    // Nothing local discovered and nothing pinned → the cloud catalog stands.
    if models.is_empty() && pinned.is_none() {
        return cloud;
    }

    let (current_id, switchable) = match &pinned {
        // Env-pinned local model: surface it (leading the list if discovery
        // missed it) and mark the catalog non-switchable.
        Some(p) => {
            if seen.insert(p.clone()) {
                let desc = runtimes
                    .iter()
                    .find(|r| r.is_available())
                    .and_then(|r| r.description());
                models.insert(
                    0,
                    ModelEntry {
                        name: p.clone(),
                        id: p.clone(),
                        description: desc,
                        kind: ModelKind::Local,
                    },
                );
            }
            (Some(p.clone()), false)
        }
        // No pin: agent is on cloud — keep its current/switchable as-is.
        None => (cloud.current_id.clone(), cloud.switchable),
    };

    // Append the cloud catalog, explicitly tagged Cloud.
    models.extend(cloud.models.into_iter().map(|m| ModelEntry {
        kind: ModelKind::Cloud,
        ..m
    }));

    ModelCatalog {
        models,
        current_id,
        switchable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        for k in [
            "COPILOT_PROVIDER_BASE_URL",
            "COPILOT_MODEL",
            "COPILOT_OFFLINE",
            "OPENAI_API_BASE",
            "OPENAI_BASE_URL",
        ] {
            std::env::remove_var(k);
        }
    }

    fn cloud_catalog() -> ModelCatalog {
        ModelCatalog {
            models: vec![
                ModelEntry {
                    id: "claude-sonnet-4.6".into(),
                    name: "Claude Sonnet 4.6".into(),
                    description: None,
                    kind: ModelKind::Cloud,
                },
                ModelEntry {
                    id: "gpt-5.5".into(),
                    name: "GPT-5.5".into(),
                    description: None,
                    kind: ModelKind::Cloud,
                },
            ],
            current_id: Some("claude-sonnet-4.6".into()),
            switchable: true,
        }
    }

    fn discover_against(response: &'static str) -> String {
        use std::io::{Read, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..2 {
                if let Ok((mut sock, _)) = listener.accept() {
                    let mut buf = [0u8; 1024];
                    let _ = sock.read(&mut buf);
                    let _ = sock.write_all(response.as_bytes());
                } else {
                    break;
                }
            }
        });

        let base = format!("http://127.0.0.1:{port}/v1");
        std::thread::sleep(std::time::Duration::from_millis(200));
        base
    }

    /// A controllable in-memory runtime so aggregation tests don't depend on
    /// which local daemons happen to be running on the test host.
    struct MockRuntime {
        id: RuntimeId,
        available: bool,
        models: Vec<String>,
        desc: Option<String>,
    }
    impl ModelRuntime for MockRuntime {
        fn id(&self) -> RuntimeId {
            self.id
        }
        fn display_name(&self) -> &'static str {
            "Mock"
        }
        fn is_available(&self) -> bool {
            self.available
        }
        fn list_models(&self) -> Vec<String> {
            self.models.clone()
        }
        fn description(&self) -> Option<String> {
            self.desc.clone()
        }
    }

    #[test]
    fn runtime_id_kind_maps_local_and_cloud() {
        assert_eq!(RuntimeId::Cloud.kind(), ModelKind::Cloud);
        assert_eq!(RuntimeId::Foundry.kind(), ModelKind::Local);
        assert_eq!(RuntimeId::Ollama.kind(), ModelKind::Local);
    }

    #[test]
    fn aggregate_without_pin_passes_cloud_through() {
        let cloud = cloud_catalog();
        assert_eq!(aggregate_with(cloud.clone(), None, vec![]), cloud);
    }

    #[test]
    fn aggregate_without_pin_surfaces_running_local_models() {
        // Zero-config discovery: a running local runtime's models show up even
        // with no env pin; the cloud current/switchable are preserved.
        let resolved = aggregate_with(
            cloud_catalog(),
            None,
            vec![Box::new(MockRuntime {
                id: RuntimeId::Ollama,
                available: true,
                models: vec!["llama3".into()],
                desc: Some("Ollama".into()),
            })],
        );
        assert!(resolved
            .models
            .iter()
            .any(|m| m.id == "llama3" && m.kind == ModelKind::Local));
        assert!(resolved
            .models
            .iter()
            .any(|m| m.id == "claude-sonnet-4.6" && m.kind == ModelKind::Cloud));
        // No pin → agent is on cloud; current + switchable unchanged.
        assert_eq!(resolved.current_id.as_deref(), Some("claude-sonnet-4.6"));
        assert!(resolved.switchable);
    }

    #[test]
    fn aggregate_with_dead_endpoint_falls_back_to_pinned() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        // No live endpoint → discovery empty → just the pinned local + cloud.
        std::env::set_var("COPILOT_PROVIDER_BASE_URL", "http://127.0.0.1:1/v1");
        let resolved = aggregate_with(
            cloud_catalog(),
            Some("qwen2.5-coder-7b".into()),
            vec![Box::new(FoundryRuntime::from_env())],
        );

        assert_eq!(resolved.models.len(), 3);
        assert_eq!(resolved.models[0].id, "qwen2.5-coder-7b");
        assert_eq!(resolved.models[0].kind, ModelKind::Local);
        assert_eq!(resolved.current_id.as_deref(), Some("qwen2.5-coder-7b"));
        assert!(resolved.models[0]
            .description
            .as_deref()
            .unwrap_or_default()
            .contains("127.0.0.1:1"));
        assert!(!resolved.switchable);
        clear_env();
    }

    #[test]
    fn aggregate_merges_multiple_runtimes_and_dedups() {
        // Two available local runtimes + an unavailable one; the pinned model is
        // already advertised by a runtime, so it is not duplicated.
        let resolved = aggregate_with(
            cloud_catalog(),
            Some("llama3".into()),
            vec![
                Box::new(MockRuntime {
                    id: RuntimeId::Ollama,
                    available: true,
                    models: vec!["llama3".into(), "qwen".into()],
                    desc: Some("Ollama".into()),
                }),
                Box::new(MockRuntime {
                    id: RuntimeId::Foundry,
                    available: true,
                    models: vec!["qwen".into(), "phi".into()], // "qwen" dups → dropped
                    desc: Some("Foundry".into()),
                }),
                Box::new(MockRuntime {
                    id: RuntimeId::Foundry,
                    available: false,
                    models: vec!["never".into()],
                    desc: None,
                }),
            ],
        );

        // llama3, qwen, phi (deduped) local + 2 cloud = 5; pinned present so no
        // extra insert.
        let local_ids: Vec<_> = resolved
            .models
            .iter()
            .filter(|m| m.kind == ModelKind::Local)
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(local_ids, vec!["llama3", "qwen", "phi"]);
        assert_eq!(resolved.models.len(), 5);
        assert_eq!(resolved.current_id.as_deref(), Some("llama3"));
        // The unavailable runtime contributed nothing.
        assert!(!resolved.models.iter().any(|m| m.id == "never"));
    }

    #[test]
    fn foundry_runtime_unavailable_without_url() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        std::env::set_var("COPILOT_OFFLINE", "true");
        let rt = FoundryRuntime::from_env();
        assert!(
            !rt.is_available(),
            "offline-only with no URL is not routable"
        );
        assert!(rt.provider_config("m").is_none());
        clear_env();
    }

    #[test]
    fn foundry_runtime_uses_cached_models_when_available() {
        let rt = FoundryRuntime {
            cfg: None,
            cached_models: vec!["foundry-local-model".into()],
        };

        assert!(rt.is_available());
        assert_eq!(rt.list_models(), vec!["foundry-local-model".to_string()]);
        assert_eq!(rt.description().as_deref(), Some("Foundry Local"));
    }

    #[test]
    fn foundry_base_url_from_urls_appends_v1_once() {
        let urls = vec!["http://127.0.0.1:5000/".to_string()];
        assert_eq!(
            foundry_base_url_from_urls(&urls).as_deref(),
            Some("http://127.0.0.1:5000/v1")
        );
    }

    #[test]
    fn foundry_runtime_detects_local_models_from_openai_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        let body = r#"{"data":[{"id":"foundry-local-model","object":"model"}],"object":"list"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let response: &'static str = Box::leak(response.into_boxed_str());
        let base = discover_against(response);
        std::env::set_var("OPENAI_API_BASE", &base);
        let rt = FoundryRuntime::detect();
        assert!(rt.is_available());
        assert_eq!(rt.list_models(), vec!["foundry-local-model".to_string()]);
        assert_eq!(
            rt.provider_config("picked").unwrap().base_url.as_deref(),
            Some(base.as_str())
        );
        clear_env();
    }

    #[test]
    fn foundry_provider_config_overrides_model() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        clear_env();
        std::env::set_var("COPILOT_PROVIDER_BASE_URL", "http://127.0.0.1:5/v1");
        std::env::set_var("COPILOT_MODEL", "env-model");
        let cfg = FoundryRuntime::from_env()
            .provider_config("picked")
            .unwrap();
        assert_eq!(cfg.model.as_deref(), Some("picked"));
        assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:5/v1"));
        clear_env();
    }
}
