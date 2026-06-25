//! Codex agent implementation.
//!
//! Thin for now — Codex inherits the default [`Agent`] behavior. Override
//! per-agent methods here as Codex-specific logic is migrated off the
//! scattered `match agent_id` sites.

use super::Agent;

pub struct CodexAgent;

impl Agent for CodexAgent {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn login_subcommand(&self) -> &'static str {
        "auth"
    }

    fn probe_credential_native(&self) -> bool {
        std::env::var("OPENAI_API_KEY").is_ok() || super::user_home().join(".codex").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn openai_api_key_satisfies_credential_probe() {
        let _g = ENV_LOCK.lock().unwrap();
        let had = std::env::var("OPENAI_API_KEY").ok();
        std::env::set_var("OPENAI_API_KEY", "sk-test");
        // probe_credential() should reach the native check (codex has no
        // auth_check_command) and see the env var.
        assert!(CodexAgent.probe_credential());
        match had {
            Some(v) => std::env::set_var("OPENAI_API_KEY", v),
            None => std::env::remove_var("OPENAI_API_KEY"),
        }
    }
}
