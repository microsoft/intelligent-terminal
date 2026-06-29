//! LLM-provider abstraction — the inference-backend axis.
//!
//! This is the orthogonal counterpart to [`crate::agent`] (the *agent
//! provider* axis: which CLI drives the conversation). The **LLM provider**
//! axis answers a different question: *which inference backend actually serves
//! the tokens* — GitHub's hosted models, a local Foundry Local / Ollama
//! endpoint, or any OpenAI-compatible BYOK endpoint.
//!
//! ## Why a dedicated layer
//!
//! Today BYOK is expressed only as a scatter of `COPILOT_PROVIDER_*` env
//! lookups inside `agent/copilot.rs`. That couples two independent dimensions
//! (agent vs. backend) and hard-codes copilot's env-var *names* as if they
//! were universal. [`LlmProviderConfig`] models the backend **generically**;
//! each [`crate::agent::Agent`] then translates that generic config into the
//! concrete env contract *its* CLI understands (see
//! `crate::agent::Agent::byok_env`). Adding a provider source (settings,
//! auto-discovery) or another agent's contract becomes a localized change.
//!
//! ## Current source: the `COPILOT_PROVIDER_*` environment
//!
//! Per the locked scope, the config *source* is still the environment contract
//! copilot already documents, plus the standard OpenAI base-url aliases used by
//! local providers:
//!
//! | env var                      | meaning                                  |
//! |------------------------------|------------------------------------------|
//! | `COPILOT_PROVIDER_BASE_URL`  | OpenAI-compatible endpoint base URL       |
//! | `COPILOT_PROVIDER_API_KEY`   | bearer key for that endpoint              |
//! | `COPILOT_PROVIDER_TYPE`      | provider flavor (e.g. `openai`)           |
//! | `COPILOT_MODEL`              | model id to pin every request to          |
//! | `COPILOT_OFFLINE`            | `true`/`1` → force air-gapped local use   |
//! | `OPENAI_API_BASE` / `OPENAI_BASE_URL` | aliases for the base URL         |
//!
//! [`LlmProviderConfig::from_env`] reads them into the generic shape. When the
//! source later moves to IT settings, only `from_env` changes; the translation
//! (`Agent::byok_env`) and the injection point (`crate::protocol::acp::spawn`)
//! stay put.

use std::env;
use std::net::ToSocketAddrs;

/// Generic, agent-neutral description of an LLM inference backend.
///
/// "Generic" means it carries no agent's env-var *names* — only the semantic
/// fields. [`crate::agent::Agent::byok_env`] maps these onto each CLI's
/// concrete contract.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LlmProviderConfig {
    /// OpenAI-compatible endpoint base URL (e.g. `http://127.0.0.1:59993/v1`).
    /// Empty/absent means "no custom endpoint configured".
    pub base_url: Option<String>,
    /// Bearer key for the endpoint. Local providers often accept any value.
    pub api_key: Option<String>,
    /// Provider flavor hint (e.g. `openai`). `None` lets the agent default it.
    pub provider_type: Option<String>,
    /// Model id every request should be pinned to.
    pub model: Option<String>,
    /// Force air-gapped operation against a local provider.
    pub offline: bool,
}

impl LlmProviderConfig {
    /// Read the active BYOK config from the process environment.
    ///
    /// Blank/whitespace-only values are normalized to `None` so a stray empty
    /// env var never looks like a configured field.
    pub fn from_env() -> Self {
        Self {
            base_url: trimmed_env("COPILOT_PROVIDER_BASE_URL")
                .or_else(|| trimmed_env("OPENAI_API_BASE"))
                .or_else(|| trimmed_env("OPENAI_BASE_URL")),
            api_key: trimmed_env("COPILOT_PROVIDER_API_KEY"),
            provider_type: trimmed_env("COPILOT_PROVIDER_TYPE"),
            model: trimmed_env("COPILOT_MODEL"),
            offline: env_is_truthy("COPILOT_OFFLINE"),
        }
    }

    /// Whether a BYOK provider is actually configured.
    ///
    /// Mirrors copilot CLI's own trigger: a non-empty endpoint base URL selects
    /// a custom provider, and `COPILOT_OFFLINE` forces local operation. Either
    /// means "the user has brought their own model".
    pub fn is_active(&self) -> bool {
        self.base_url.as_deref().is_some_and(|s| !s.is_empty()) || self.offline
    }
}

/// The user's last `/model` pick, persisted so it survives the agent-CLI
/// respawn that a cloud↔local switch requires.
///
/// `COPILOT_MODEL` / the BYOK provider env are read by the agent CLI only at
/// process start, so switching across the cloud/local boundary means
/// reconfiguring the env and respawning. This selection is the bridge: the
/// helper writes it on a pick, then triggers the stack restart; the freshly
/// spawned master reads it back in [`spawn_provider`] and applies the right
/// env to the new agent CLI.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProviderSelection {
    /// Which runtime serves the picked model. `Cloud` forces the BYOK env
    /// *off* on the next spawn; a local runtime (`Ollama`/`Foundry`) supplies
    /// the provider endpoint to route to.
    pub runtime: crate::model_runtime::RuntimeId,
    /// The picked model id (the `COPILOT_MODEL` to pin for local; the cloud
    /// catalog id to re-select for cloud).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Location of the persisted [`ProviderSelection`] under the package-private
/// state root. `None` when there's no resolvable state dir (shouldn't happen in
/// a packaged process; unpackaged dev falls back to the bare LocalAppData dir).
fn selection_path() -> Option<std::path::PathBuf> {
    crate::runtime_paths::intelligent_terminal_root().map(|r| r.join("provider-selection.json"))
}

/// Read the persisted provider selection, or `None` when absent/unreadable.
/// A missing or malformed file is treated as "no explicit selection" so the
/// spawn falls back to ambient env behavior.
pub fn load_selection() -> Option<ProviderSelection> {
    let path = selection_path()?;
    let raw = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// Persist the provider selection atomically (temp file + rename), so a master
/// spawn that reads it concurrently never sees a half-written file.
pub fn save_selection(sel: &ProviderSelection) -> std::io::Result<()> {
    let path = selection_path().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no resolvable state root for provider selection",
        )
    })?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let json = serde_json::to_string_pretty(sel).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, json)?;
    match std::fs::rename(&tmp, &path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// What the BYOK env should look like on the *next* agent-CLI spawn, resolved
/// from the persisted [`ProviderSelection`] overlaid on the ambient env.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnProvider {
    /// Inject this local BYOK config onto the child (provider env + pinned model).
    Local(LlmProviderConfig),
    /// Force cloud: strip any inherited BYOK provider env from the child so the
    /// agent CLI talks to its hosted backend, ignoring an ambient local config.
    Cloud,
    /// No explicit selection and no active ambient BYOK config — leave the
    /// child's inherited env untouched (the pre-BYOK default behavior).
    Inherit,
}

/// Resolve the spawn-time provider plan: the persisted selection wins over the
/// ambient env, falling back to the env when no selection is recorded.
///
/// * Cloud selection → [`SpawnProvider::Cloud`] (clear inherited BYOK env).
/// * Local selection → ambient provider config with the model overridden to the
///   pick; degrades to `Inherit` if no provider endpoint is configured (you
///   can't route to a local model without one).
/// * No selection → the prior behavior: `Local` when the env is active, else
///   `Inherit`.
pub fn spawn_provider() -> SpawnProvider {
    let selection = load_selection();
    // Resolve the selected runtime's provider config (Ollama's fixed endpoint,
    // Foundry's env endpoint). `None` for cloud / no model / unconfigured local.
    let runtime_cfg = selection.as_ref().and_then(|sel| {
        let model = sel.model.as_deref().filter(|m| !m.trim().is_empty())?;
        crate::model_runtime::runtime_provider_config(sel.runtime, model)
    });
    resolve_spawn_provider(selection, runtime_cfg, LlmProviderConfig::from_env())
}

/// Pure resolution of the spawn-time provider plan from an explicit selection,
/// the runtime-resolved provider config, and the ambient env config. Split out
/// from [`spawn_provider`] so it can be unit tested without touching the
/// process-shared on-disk selection file or live runtimes.
fn resolve_spawn_provider(
    selection: Option<ProviderSelection>,
    runtime_cfg: Option<LlmProviderConfig>,
    env_cfg: LlmProviderConfig,
) -> SpawnProvider {
    use crate::model_runtime::RuntimeId;
    match selection {
        Some(sel) if sel.runtime == RuntimeId::Cloud => SpawnProvider::Cloud,
        Some(sel) => {
            // Prefer the runtime-resolved config; fall back to the ambient env
            // with the picked model overlaid (so a stale-but-active env still
            // routes when the runtime can't supply a config of its own).
            let cfg = runtime_cfg.or_else(|| {
                let mut cfg = env_cfg;
                if let Some(model) = sel.model.filter(|m| !m.trim().is_empty()) {
                    cfg.model = Some(model);
                }
                Some(cfg)
            });
            match cfg {
                Some(cfg) if cfg.is_active() => SpawnProvider::Local(cfg),
                _ => SpawnProvider::Inherit,
            }
        }
        None => {
            if env_cfg.is_active() {
                SpawnProvider::Local(env_cfg)
            } else {
                SpawnProvider::Inherit
            }
        }
    }
}

/// Read an env var, trimming whitespace and mapping empty to `None`.
fn trimmed_env(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `true` when the env var is set to a truthy value (`true`/`1`, case-insensitive).
fn env_is_truthy(key: &str) -> bool {
    env::var(key)
        .map(|v| {
            let v = v.trim();
            v.eq_ignore_ascii_case("true") || v == "1"
        })
        .unwrap_or(false)
}

/// A model advertised by a local OpenAI-compatible endpoint (Foundry Local,
/// Ollama, …). Only the fields the picker needs are kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredModel {
    /// The model id to pin requests to (e.g. `qwen2.5-coder-7b-instruct-...`).
    pub id: String,
}

/// How long the blocking discovery probe is allowed to take end-to-end. The
/// caller (`resolve_models`) runs on the app event loop, so this stays short;
/// a slow/absent local endpoint degrades gracefully to "no local models".
const DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(800);

/// Probe a local OpenAI-compatible endpoint for its model catalog.
///
/// Issues a blocking `GET {base_url}/models` over a raw TCP socket (no TLS — the
/// supported targets are `http://127.0.0.1:<port>/v1`) and parses the standard
/// `{"data":[{"id":...}]}` response. Any failure (no endpoint, timeout, non-200,
/// unparseable body) yields an empty `Vec` so the picker simply shows no local
/// models rather than erroring.
///
/// Intentionally dependency-free (hand-rolled HTTP) to avoid pulling a full
/// HTTP client + its transitive crates into the wta tree.
pub fn discover_local_models(base_url: &str) -> Vec<DiscoveredModel> {
    discover_local_models_inner(base_url).unwrap_or_default()
}

fn discover_local_models_inner(base_url: &str) -> Option<Vec<DiscoveredModel>> {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    let base = base_url.trim().trim_end_matches('/');
    // Strip scheme; we only speak plain HTTP to localhost.
    let after_scheme = base.strip_prefix("http://").or_else(|| base.strip_prefix("https://"))?;
    // Split authority (host:port) from any base path (e.g. `/v1`).
    let (authority, base_path) = match after_scheme.find('/') {
        Some(i) => (&after_scheme[..i], &after_scheme[i..]),
        None => (after_scheme, ""),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().ok()?),
        None => (authority, 80),
    };
    let path = format!("{}/models", base_path.trim_end_matches('/'));

    let addr = format!("{host}:{port}")
        .to_socket_addrs()
        .ok()?
        .next()?;
    let mut stream = TcpStream::connect_timeout(&addr, DISCOVERY_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(DISCOVERY_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(DISCOVERY_TIMEOUT)).ok()?;

    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).ok()?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).ok()?;

    let split = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let headers = String::from_utf8_lossy(&raw[..split]).to_ascii_lowercase();
    let body_bytes = &raw[split + 4..];
    let body = if headers.contains("transfer-encoding: chunked") {
        dechunk(body_bytes)
    } else {
        body_bytes.to_vec()
    };

    let json: serde_json::Value = serde_json::from_slice(&body).ok()?;
    let data = json.get("data")?.as_array()?;
    let models: Vec<DiscoveredModel> = data
        .iter()
        .filter_map(|m| m.get("id").and_then(|v| v.as_str()))
        .filter(|id| !id.is_empty())
        .map(|id| DiscoveredModel { id: id.to_string() })
        .collect();
    Some(models)
}

/// Decode an HTTP/1.1 chunked transfer-encoded body. Best-effort: stops at the
/// terminating zero-length chunk or when the buffer is exhausted.
fn dechunk(mut buf: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let line_end = match buf.windows(2).position(|w| w == b"\r\n") {
            Some(i) => i,
            None => break,
        };
        let size_str = String::from_utf8_lossy(&buf[..line_end]);
        // Chunk size may carry extensions after a `;` — ignore them.
        let size_hex = size_str.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let data_start = line_end + 2;
        let data_end = data_start + size;
        if data_end > buf.len() {
            out.extend_from_slice(&buf[data_start..]);
            break;
        }
        out.extend_from_slice(&buf[data_start..data_end]);
        // Skip the chunk and its trailing CRLF.
        buf = &buf[(data_end + 2).min(buf.len())..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // The BYOK env vars are process-global; serialize tests that mutate them.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_env() {
        for k in [
            "COPILOT_PROVIDER_BASE_URL",
            "COPILOT_PROVIDER_API_KEY",
            "COPILOT_PROVIDER_TYPE",
            "COPILOT_MODEL",
            "COPILOT_OFFLINE",
            "OPENAI_API_BASE",
            "OPENAI_BASE_URL",
        ] {
            env::remove_var(k);
        }
    }

    #[test]
    fn from_env_reads_all_fields_and_trims() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("COPILOT_PROVIDER_BASE_URL", "  http://127.0.0.1:59993/v1 ");
        env::set_var("COPILOT_PROVIDER_API_KEY", "foundry-local-no-auth");
        env::set_var("COPILOT_PROVIDER_TYPE", "openai");
        env::set_var("COPILOT_MODEL", " qwen2.5-coder-7b ");
        env::set_var("COPILOT_OFFLINE", "true");

        let cfg = LlmProviderConfig::from_env();
        assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:59993/v1"));
        assert_eq!(cfg.api_key.as_deref(), Some("foundry-local-no-auth"));
        assert_eq!(cfg.provider_type.as_deref(), Some("openai"));
        assert_eq!(cfg.model.as_deref(), Some("qwen2.5-coder-7b"));
        assert!(cfg.offline);
        assert!(cfg.is_active());
        clear_env();
    }

    #[test]
    fn blank_values_normalize_to_none_and_inactive() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("COPILOT_PROVIDER_BASE_URL", "   ");
        env::set_var("COPILOT_OFFLINE", "false");
        let cfg = LlmProviderConfig::from_env();
        assert_eq!(cfg.base_url, None, "whitespace base URL must normalize to None");
        assert!(!cfg.offline);
        assert!(!cfg.is_active(), "blank URL + falsey offline is not BYOK");
        clear_env();
    }

    #[test]
    fn offline_alone_activates_byok() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("COPILOT_OFFLINE", "1");
        assert!(LlmProviderConfig::from_env().is_active());
        clear_env();
    }

    #[test]
    fn empty_env_is_inactive_default() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        let cfg = LlmProviderConfig::from_env();
        assert_eq!(cfg, LlmProviderConfig::default());
        assert!(!cfg.is_active());
        clear_env();
    }

    #[test]
    fn openai_alias_base_urls_are_treated_as_active() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_env();
        env::set_var("OPENAI_API_BASE", " http://127.0.0.1:59993/v1 ");
        let cfg = LlmProviderConfig::from_env();
        assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:59993/v1"));
        assert!(cfg.is_active());
        clear_env();
    }

    #[test]
    fn dechunk_decodes_chunked_body() {
        // "Hello" + " World" across two chunks, terminated by a 0-length chunk.
        let body = b"5\r\nHello\r\n6\r\n World\r\n0\r\n\r\n";
        assert_eq!(dechunk(body), b"Hello World");
    }

    /// Spin up a one-shot localhost HTTP server returning `response`, then point
    /// `discover_local_models` at it. Returns the discovered ids.
    fn discover_against(response: &'static str) -> Vec<String> {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                let _ = sock.read(&mut buf); // consume the request line/headers
                let _ = sock.write_all(response.as_bytes());
            }
        });
        let base = format!("http://127.0.0.1:{port}/v1");
        let models = discover_local_models(&base);
        let _ = handle.join();
        models.into_iter().map(|m| m.id).collect()
    }

    #[test]
    fn discover_parses_openai_models_response() {
        // Mirrors Foundry Local's actual /v1/models shape.
        let body = r#"{"data":[{"id":"qwen2.5-coder-7b-instruct-generic-cpu:4","object":"model"},{"id":"phi-3.5-mini","object":"model"}],"object":"list"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        // Leak to get 'static for the thread closure (test-only, tiny).
        let response: &'static str = Box::leak(response.into_boxed_str());
        let ids = discover_against(response);
        assert_eq!(
            ids,
            vec![
                "qwen2.5-coder-7b-instruct-generic-cpu:4".to_string(),
                "phi-3.5-mini".to_string()
            ]
        );
    }

    #[test]
    fn discover_handles_chunked_response() {
        let body = r#"{"data":[{"id":"local-x","object":"model"}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n",
            body.len(),
            body
        );
        let response: &'static str = Box::leak(response.into_boxed_str());
        let ids = discover_against(response);
        assert_eq!(ids, vec!["local-x".to_string()]);
    }

    #[test]
    fn discover_returns_empty_on_no_endpoint() {
        // Port 1 has no listener — connect fails fast, yields empty.
        assert!(discover_local_models("http://127.0.0.1:1/v1").is_empty());
    }

    fn local_cfg() -> LlmProviderConfig {
        LlmProviderConfig {
            base_url: Some("http://127.0.0.1:5/v1".to_string()),
            api_key: Some("x".to_string()),
            provider_type: Some("openai".to_string()),
            model: Some("env-model".to_string()),
            offline: false,
        }
    }

    #[test]
    fn provider_selection_roundtrips_through_json() {
        let sel = ProviderSelection {
            runtime: crate::model_runtime::RuntimeId::Ollama,
            model: Some("qwen2.5-coder-7b".to_string()),
        };
        let json = serde_json::to_string(&sel).unwrap();
        let back: ProviderSelection = serde_json::from_str(&json).unwrap();
        assert_eq!(sel, back);
    }

    #[test]
    fn provider_selection_omits_absent_model() {
        let sel = ProviderSelection {
            runtime: crate::model_runtime::RuntimeId::Cloud,
            model: None,
        };
        let json = serde_json::to_string(&sel).unwrap();
        assert!(!json.contains("model"), "absent model must be skipped: {json}");
    }

    #[test]
    fn cloud_selection_forces_cloud_ignoring_active_env() {
        // Even with an active local env, an explicit cloud pick wins.
        let sel = Some(ProviderSelection {
            runtime: crate::model_runtime::RuntimeId::Cloud,
            model: Some("gpt-5.5".to_string()),
        });
        assert_eq!(
            resolve_spawn_provider(sel, None, local_cfg()),
            SpawnProvider::Cloud
        );
    }

    #[test]
    fn local_selection_prefers_runtime_config() {
        // The runtime-resolved config wins over the ambient env fallback.
        let sel = Some(ProviderSelection {
            runtime: crate::model_runtime::RuntimeId::Ollama,
            model: Some("picked-model".to_string()),
        });
        let runtime_cfg = Some(LlmProviderConfig {
            base_url: Some("http://127.0.0.1:11434/v1".to_string()),
            api_key: Some("ollama".to_string()),
            provider_type: Some("openai".to_string()),
            model: Some("picked-model".to_string()),
            offline: false,
        });
        match resolve_spawn_provider(sel, runtime_cfg, local_cfg()) {
            SpawnProvider::Local(cfg) => {
                assert_eq!(cfg.model.as_deref(), Some("picked-model"));
                assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:11434/v1"));
            }
            other => panic!("expected Local, got {other:?}"),
        }
    }

    #[test]
    fn local_selection_falls_back_to_env_model_when_runtime_has_none() {
        // No runtime config (e.g. Foundry with no env endpoint resolved here) →
        // fall back to the ambient env with the picked model overlaid.
        let sel = Some(ProviderSelection {
            runtime: crate::model_runtime::RuntimeId::Foundry,
            model: Some("picked-model".to_string()),
        });
        match resolve_spawn_provider(sel, None, local_cfg()) {
            SpawnProvider::Local(cfg) => {
                assert_eq!(cfg.model.as_deref(), Some("picked-model"));
                assert_eq!(cfg.base_url.as_deref(), Some("http://127.0.0.1:5/v1"));
            }
            other => panic!("expected Local, got {other:?}"),
        }
    }

    #[test]
    fn local_selection_without_endpoint_degrades_to_inherit() {
        // A local pick is meaningless without a provider endpoint configured
        // (no runtime config and an inactive ambient env).
        let sel = Some(ProviderSelection {
            runtime: crate::model_runtime::RuntimeId::Foundry,
            model: Some("picked-model".to_string()),
        });
        assert_eq!(
            resolve_spawn_provider(sel, None, LlmProviderConfig::default()),
            SpawnProvider::Inherit
        );
    }

    #[test]
    fn no_selection_falls_back_to_env() {
        assert_eq!(
            resolve_spawn_provider(None, None, local_cfg()),
            SpawnProvider::Local(local_cfg())
        );
        assert_eq!(
            resolve_spawn_provider(None, None, LlmProviderConfig::default()),
            SpawnProvider::Inherit
        );
    }
}
