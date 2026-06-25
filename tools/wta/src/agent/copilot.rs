//! GitHub Copilot agent implementation.

use std::env;

use super::Agent;
use crate::llm_provider::LlmProviderConfig;

/// The GitHub Copilot CLI agent.
///
/// Copilot is the only agent WTA can auto-install, and the only one whose
/// BYOK provider contract is wired up here (the `COPILOT_PROVIDER_*` /
/// `COPILOT_OFFLINE` env vars Copilot CLI reads). See [`Agent::auth_needed`].
pub struct CopilotAgent;

impl Agent for CopilotAgent {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn can_auto_install(&self) -> bool {
        true
    }

    fn drives_interactive_signin(&self) -> bool {
        // WTA drives Copilot's device-flow sign-in itself (the SignIn setup
        // option), unlike other agents where the user signs in externally.
        true
    }

    fn probe_credential_native(&self) -> bool {
        // Copilot CLI stores its credential in the Windows Credential Manager
        // under a `copilot-cli` target; cmdkey is the cheapest way to check.
        std::process::Command::new("cmd")
            .args(["/C", "cmdkey /list | findstr /i copilot-cli"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false)
    }

    fn auth_needed(&self) -> bool {
        // Two-axis auth: when the user has configured their own LLM provider
        // (a local Ollama / Foundry Local endpoint, or any BYOK endpoint),
        // Copilot CLI does not require a GitHub sign-in to run — per the BYOK
        // docs, "When using your own model provider, GitHub authentication is
        // not required." Sign-in stays *optional* (it unlocks /delegate, Code
        // Search, …), so we simply drop the hard gate here.
        if byok_provider_configured() {
            return false;
        }
        !self.probe_credential()
    }

    fn resolve_models(&self, acp: super::ModelCatalog) -> super::ModelCatalog {
        // Under BYOK, Copilot CLI's ACP `new_session` still advertises its full
        // cloud catalog (auto / claude / gpt / gemini) with a cloud
        // `current_model_id`, completely decoupled from the actual inference
        // routing — `COPILOT_MODEL` pins the model at the HTTP layer only. So
        // the ACP list is a lie in this mode: surface the real local model and
        // mark it non-switchable (changing it requires restarting Copilot with
        // a different `COPILOT_MODEL`, not `session/set_model`).
        let Some(model_id) = byok_model() else {
            return acp;
        };
        let provider = env::var("COPILOT_PROVIDER_BASE_URL").unwrap_or_default();
        let provider = provider.trim();
        let description = (!provider.is_empty()).then(|| format!("Local provider · {provider}"));
        super::ModelCatalog {
            models: vec![super::ModelEntry {
                id: model_id.clone(),
                name: format!("{model_id} (local)"),
                description,
            }],
            current_id: Some(model_id),
            switchable: false,
        }
    }

    fn supports_byok(&self) -> bool {
        true
    }

    fn byok_env(&self, cfg: &LlmProviderConfig) -> Vec<(String, String)> {
        // Translate the generic provider config into copilot CLI's concrete
        // BYOK env contract. We only emit the fields the user actually set, so
        // the spawned child sees exactly the values present in the source (no
        // synthetic empties that would otherwise look "configured").
        let mut env = Vec::new();
        if let Some(base_url) = &cfg.base_url {
            env.push(("COPILOT_PROVIDER_BASE_URL".to_string(), base_url.clone()));
        }
        if let Some(api_key) = &cfg.api_key {
            env.push(("COPILOT_PROVIDER_API_KEY".to_string(), api_key.clone()));
        }
        if let Some(provider_type) = &cfg.provider_type {
            env.push(("COPILOT_PROVIDER_TYPE".to_string(), provider_type.clone()));
        }
        if let Some(model) = &cfg.model {
            env.push(("COPILOT_MODEL".to_string(), model.clone()));
        }
        if cfg.offline {
            env.push(("COPILOT_OFFLINE".to_string(), "true".to_string()));
        }
        env
    }
}

/// The local model Copilot CLI is pinned to via BYOK, or `None` when BYOK is
/// not configured (so the ACP-advertised cloud catalog stays authoritative).
///
/// `COPILOT_MODEL` is the model id Copilot CLI routes every request to when a
/// custom provider is set; we treat it as the single source of truth for the
/// "what model am I really on" display in BYOK mode.
fn byok_model() -> Option<String> {
    if !byok_provider_configured() {
        return None;
    }
    env::var("COPILOT_MODEL")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// True when the environment pins Copilot CLI to a user-supplied LLM provider.
///
/// Thin wrapper over [`LlmProviderConfig::from_env`] + [`LlmProviderConfig::is_active`]
/// so copilot's auth gate and the generic provider model share one definition
/// of "BYOK is configured": a non-empty `COPILOT_PROVIDER_BASE_URL` selects a
/// custom provider, and `COPILOT_OFFLINE=true` forces air-gapped operation.
fn byok_provider_configured() -> bool {
    LlmProviderConfig::from_env().is_active()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The BYOK env vars are process-global; serialize the tests that mutate
    // them so they don't race each other.
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn clear_byok_env() {
        env::remove_var("COPILOT_PROVIDER_BASE_URL");
        env::remove_var("COPILOT_OFFLINE");
        env::remove_var("COPILOT_MODEL");
    }

    #[test]
    fn base_url_disables_auth_gate() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_byok_env();
        env::set_var("COPILOT_PROVIDER_BASE_URL", "http://localhost:11434");
        assert!(
            !CopilotAgent.auth_needed(),
            "a configured BYOK provider must drop the GitHub auth gate"
        );
        clear_byok_env();
    }

    #[test]
    fn offline_disables_auth_gate() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_byok_env();
        env::set_var("COPILOT_OFFLINE", "true");
        assert!(!CopilotAgent.auth_needed());
        env::set_var("COPILOT_OFFLINE", "1");
        assert!(!CopilotAgent.auth_needed());
        clear_byok_env();
    }

    #[test]
    fn blank_byok_values_do_not_count() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_byok_env();
        env::set_var("COPILOT_PROVIDER_BASE_URL", "   ");
        env::set_var("COPILOT_OFFLINE", "false");
        assert!(
            !byok_provider_configured(),
            "blank base URL and falsey offline flag must not be treated as BYOK"
        );
        clear_byok_env();
    }

    fn sample_acp_catalog() -> super::super::ModelCatalog {
        super::super::ModelCatalog {
            models: vec![
                super::super::ModelEntry {
                    id: "claude-sonnet-4.6".into(),
                    name: "Claude Sonnet 4.6".into(),
                    description: None,
                },
                super::super::ModelEntry {
                    id: "gpt-5.5".into(),
                    name: "GPT-5.5".into(),
                    description: None,
                },
            ],
            current_id: Some("claude-sonnet-4.6".into()),
            switchable: true,
        }
    }

    #[test]
    fn resolve_models_passes_through_without_byok() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_byok_env();
        let acp = sample_acp_catalog();
        let resolved = CopilotAgent.resolve_models(acp.clone());
        assert_eq!(
            resolved, acp,
            "without BYOK the ACP-advertised catalog is authoritative"
        );
        clear_byok_env();
    }

    #[test]
    fn resolve_models_overrides_to_pinned_local_model_under_byok() {
        let _g = ENV_LOCK.lock().unwrap();
        clear_byok_env();
        env::set_var("COPILOT_PROVIDER_BASE_URL", "http://127.0.0.1:55690/v1");
        env::set_var("COPILOT_MODEL", "qwen2.5-coder-7b");
        let resolved = CopilotAgent.resolve_models(sample_acp_catalog());
        assert_eq!(resolved.models.len(), 1, "BYOK collapses to the one real model");
        assert_eq!(resolved.models[0].id, "qwen2.5-coder-7b");
        assert_eq!(resolved.models[0].name, "qwen2.5-coder-7b (local)");
        assert_eq!(resolved.current_id.as_deref(), Some("qwen2.5-coder-7b"));
        assert!(
            !resolved.switchable,
            "the env-pinned BYOK model can't be switched at runtime"
        );
        assert!(
            resolved.models[0]
                .description
                .as_deref()
                .unwrap_or_default()
                .contains("127.0.0.1:55690"),
            "the provider URL is surfaced in the description"
        );
        clear_byok_env();
    }

    #[test]
    fn resolve_models_keeps_acp_when_byok_url_set_but_model_unnamed() {        // A base URL with no COPILOT_MODEL: we can't name the model, so leave
        // the ACP list rather than inventing an entry.
        let _g = ENV_LOCK.lock().unwrap();
        clear_byok_env();
        env::set_var("COPILOT_PROVIDER_BASE_URL", "http://127.0.0.1:55690/v1");
        let acp = sample_acp_catalog();
        let resolved = CopilotAgent.resolve_models(acp.clone());
        assert_eq!(resolved, acp);
        clear_byok_env();
    }

    #[test]
    fn byok_env_translates_generic_config_to_copilot_contract() {
        let cfg = LlmProviderConfig {
            base_url: Some("http://127.0.0.1:59993/v1".into()),
            api_key: Some("foundry-local-no-auth".into()),
            provider_type: Some("openai".into()),
            model: Some("qwen2.5-coder-7b".into()),
            offline: true,
        };
        let env = CopilotAgent.byok_env(&cfg);
        assert!(CopilotAgent.supports_byok());
        assert!(env.contains(&(
            "COPILOT_PROVIDER_BASE_URL".to_string(),
            "http://127.0.0.1:59993/v1".to_string()
        )));
        assert!(env.contains(&(
            "COPILOT_PROVIDER_API_KEY".to_string(),
            "foundry-local-no-auth".to_string()
        )));
        assert!(env.contains(&("COPILOT_PROVIDER_TYPE".to_string(), "openai".to_string())));
        assert!(env.contains(&("COPILOT_MODEL".to_string(), "qwen2.5-coder-7b".to_string())));
        assert!(env.contains(&("COPILOT_OFFLINE".to_string(), "true".to_string())));
    }

    #[test]
    fn byok_env_omits_unset_fields() {
        // Only a base URL set: no synthetic empties for the other vars, and no
        // COPILOT_OFFLINE when the flag is false.
        let cfg = LlmProviderConfig {
            base_url: Some("http://127.0.0.1:59993/v1".into()),
            ..Default::default()
        };
        let env = CopilotAgent.byok_env(&cfg);
        assert_eq!(
            env,
            vec![(
                "COPILOT_PROVIDER_BASE_URL".to_string(),
                "http://127.0.0.1:59993/v1".to_string()
            )],
            "only the configured field is emitted"
        );
    }
}
