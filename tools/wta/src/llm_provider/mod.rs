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
//! Per the locked scope, the only config *source* wired up now is the
//! environment contract copilot already documents:
//!
//! | env var                      | meaning                                  |
//! |------------------------------|------------------------------------------|
//! | `COPILOT_PROVIDER_BASE_URL`  | OpenAI-compatible endpoint base URL       |
//! | `COPILOT_PROVIDER_API_KEY`   | bearer key for that endpoint              |
//! | `COPILOT_PROVIDER_TYPE`      | provider flavor (e.g. `openai`)           |
//! | `COPILOT_MODEL`              | model id to pin every request to          |
//! | `COPILOT_OFFLINE`            | `true`/`1` → force air-gapped local use   |
//!
//! [`LlmProviderConfig::from_env`] reads them into the generic shape. When the
//! source later moves to IT settings, only `from_env` changes; the translation
//! (`Agent::byok_env`) and the injection point (`crate::protocol::acp::spawn`)
//! stay put.

use std::env;

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
            base_url: trimmed_env("COPILOT_PROVIDER_BASE_URL"),
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
}
