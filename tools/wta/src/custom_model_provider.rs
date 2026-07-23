//! Custom model provider isolation and Credential Manager resolution.

use anyhow::{bail, Context, Result};
use tokio::process::Command;

use crate::agent_registry::ByokMode;

const SHARED_BASE_URL: &str = "WTA_CUSTOM_MODEL_BASE_URL";
const SHARED_MODEL: &str = "WTA_CUSTOM_MODEL_ID";
const SHARED_CREDENTIAL_ID: &str = "WTA_CUSTOM_MODEL_CREDENTIAL_ID";

const COPILOT_BASE_URL: &str = "COPILOT_PROVIDER_BASE_URL";
const COPILOT_API_KEY: &str = "COPILOT_PROVIDER_API_KEY";
const COPILOT_PROVIDER_TYPE: &str = "COPILOT_PROVIDER_TYPE";
const COPILOT_MODEL: &str = "COPILOT_MODEL";
const COPILOT_OFFLINE: &str = "COPILOT_OFFLINE";

const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";
const OPENCODE_API_KEY: &str = "INTELLIGENT_TERMINAL_MODEL_API_KEY";
const OPENCODE_PROVIDER_ID: &str = "intelligent-terminal";

const METADATA_ENV_KEYS: &[&str] = &[
    SHARED_BASE_URL,
    SHARED_MODEL,
    SHARED_CREDENTIAL_ID,
    COPILOT_BASE_URL,
    COPILOT_API_KEY,
    COPILOT_PROVIDER_TYPE,
    COPILOT_MODEL,
    COPILOT_OFFLINE,
    OPENCODE_CONFIG_CONTENT,
];

pub(crate) struct Config {
    pub(crate) base_url: String,
    pub(crate) model: String,
    pub(crate) credential_id: Option<String>,
    pub(crate) credential_resource: &'static str,
}

impl Config {
    fn shared_from_env() -> Self {
        Self {
            base_url: trimmed_env(SHARED_BASE_URL).unwrap_or_default(),
            model: trimmed_env(SHARED_MODEL).unwrap_or_default(),
            credential_id: trimmed_env(SHARED_CREDENTIAL_ID),
            credential_resource: "IntelligentTerminal.CustomModelProvider",
        }
    }

    pub(crate) fn is_complete(&self) -> bool {
        !self.base_url.is_empty() && !self.model.is_empty()
    }

    fn resolve_api_key(&self) -> Result<Option<String>> {
        match self.credential_id.as_deref() {
            Some(id) => read_api_key(self.credential_resource, id),
            None => Ok(None),
        }
    }
}

/// Scrub trusted provider metadata from every child, then adapt the shared
/// configuration only for an agent that supports it.
pub(crate) fn configure_child(cmd: &mut Command, byok_mode: ByokMode) -> Result<()> {
    let shared = Config::shared_from_env();
    for key in METADATA_ENV_KEYS {
        cmd.env_remove(key);
    }
    cmd.env_remove(OPENCODE_API_KEY);

    if shared.is_complete() {
        match byok_mode {
            ByokMode::Unsupported => {}
            ByokMode::CopilotProviderEnvironment => configure_copilot(cmd, &shared)?,
            ByokMode::OpenCodeConfigContent => configure_opencode(cmd, &shared)?,
        }
    }
    Ok(())
}

fn configure_copilot(cmd: &mut Command, config: &Config) -> Result<()> {
    cmd.env(COPILOT_BASE_URL, &config.base_url)
        .env(COPILOT_MODEL, &config.model)
        .env(COPILOT_PROVIDER_TYPE, "openai")
        .env(COPILOT_OFFLINE, "true")
        .env_remove(COPILOT_API_KEY);
    if let Some(api_key) = config.resolve_api_key()? {
        cmd.env(COPILOT_API_KEY, api_key);
    }
    Ok(())
}

fn configure_opencode(cmd: &mut Command, config: &Config) -> Result<()> {
    let api_key = config.resolve_api_key()?;
    cmd.env(
        OPENCODE_CONFIG_CONTENT,
        render_opencode_config(config, api_key.is_some())?,
    );
    if let Some(api_key) = api_key {
        cmd.env(OPENCODE_API_KEY, api_key);
    }
    Ok(())
}

fn render_opencode_config(config: &Config, has_api_key: bool) -> Result<String> {
    let mut options = serde_json::Map::from_iter([(
        "baseURL".to_string(),
        serde_json::Value::String(config.base_url.clone()),
    )]);
    if has_api_key {
        options.insert(
            "apiKey".to_string(),
            serde_json::Value::String(format!("{{env:{OPENCODE_API_KEY}}}")),
        );
    }

    let models = serde_json::Map::from_iter([(
        config.model.clone(),
        serde_json::json!({ "name": config.model }),
    )]);
    let providers = serde_json::Map::from_iter([(
        OPENCODE_PROVIDER_ID.to_string(),
        serde_json::json!({
            "npm": "@ai-sdk/openai-compatible",
            "name": "Intelligent Terminal BYOK",
            "options": options,
            "models": models,
        }),
    )]);

    serde_json::to_string(&serde_json::json!({
        "$schema": "https://opencode.ai/config.json",
        "model": format!("{OPENCODE_PROVIDER_ID}/{}", config.model),
        "provider": providers,
    }))
    .context("failed to serialize OpenCode custom model configuration")
}

fn trimmed_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_api_key(credential_resource: &str, credential_id: &str) -> Result<Option<String>> {
    use windows_sys::Win32::Foundation::{GetLastError, ERROR_NOT_FOUND};
    use windows_sys::Win32::Security::Credentials::{
        CredFree, CredReadW, CREDENTIALW, CRED_TYPE_GENERIC,
    };

    let target: Vec<u16> = format!("{credential_resource}/{credential_id}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut credential: *mut CREDENTIALW = std::ptr::null_mut();
    if unsafe { CredReadW(target.as_ptr(), CRED_TYPE_GENERIC, 0, &mut credential) } == 0 {
        let error = unsafe { GetLastError() };
        if error == ERROR_NOT_FOUND {
            return Ok(None);
        }
        bail!("failed to read model provider credential: Win32 error {error}");
    }
    if credential.is_null() {
        bail!("Credential Manager returned a null model provider credential");
    }

    let blob_size = unsafe { (*credential).CredentialBlobSize as usize };
    let blob = unsafe { (*credential).CredentialBlob };
    if blob_size == 0 || blob.is_null() {
        unsafe { CredFree(credential.cast()) };
        bail!("model provider credential is empty");
    }
    let mut bytes = unsafe { std::slice::from_raw_parts(blob, blob_size).to_vec() };
    unsafe { CredFree(credential.cast()) };

    let api_key = std::str::from_utf8(&bytes).map(|value| value.trim().to_string());
    bytes.fill(0);
    let api_key = api_key.context("model provider credential is not valid UTF-8")?;
    if api_key.is_empty() {
        bail!("model provider credential is empty");
    }
    Ok(Some(api_key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_config_uses_shared_provider_without_persisting_secret() {
        let rendered = render_opencode_config(
            &Config {
                base_url: "https://openrouter.ai/api/v1".to_string(),
                model: "qwen/qwen3.5-9b".to_string(),
                credential_id: Some("opaque-id".to_string()),
                credential_resource: "test",
            },
            true,
        )
        .expect("OpenCode config should serialize");
        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("OpenCode config should be valid JSON");

        assert_eq!(parsed["model"], "intelligent-terminal/qwen/qwen3.5-9b");
        assert_eq!(
            parsed["provider"]["intelligent-terminal"]["options"]["baseURL"],
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(
            parsed["provider"]["intelligent-terminal"]["options"]["apiKey"],
            "{env:INTELLIGENT_TERMINAL_MODEL_API_KEY}"
        );
        assert!(!rendered.contains("opaque-id"));
    }

    #[test]
    fn requires_endpoint_and_model() {
        let complete = Config {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen3.5:9b".to_string(),
            credential_id: None,
            credential_resource: "test",
        };
        assert!(complete.is_complete());

        assert!(!Config {
            model: String::new(),
            ..complete
        }
        .is_complete());
    }

    #[test]
    fn unsupported_agent_has_provider_metadata_removed() {
        let mut cmd = Command::new("unsupported-agent");
        for key in METADATA_ENV_KEYS {
            cmd.env(key, "must-not-leak");
        }

        configure_child(&mut cmd, ByokMode::Unsupported)
            .expect("metadata scrubbing should succeed");

        let configured_env: std::collections::HashMap<_, _> = cmd.as_std().get_envs().collect();
        for key in METADATA_ENV_KEYS {
            assert_eq!(configured_env.get(std::ffi::OsStr::new(key)), Some(&None));
        }
    }
}
