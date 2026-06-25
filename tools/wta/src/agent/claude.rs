//! Claude agent implementation.
//!
//! Thin for now — Claude inherits the default [`Agent`] behavior (credential
//! probe and login command delegate to `agent_check`). When Claude's own
//! BYOK / provider contract is wired up, override [`Agent::auth_needed`] here.

use super::Agent;

pub struct ClaudeAgent;

impl Agent for ClaudeAgent {
    fn id(&self) -> &'static str {
        "claude"
    }

    fn probe_credential_native(&self) -> bool {
        let path = super::user_home().join(".claude").join(".credentials.json");
        let exists = path.exists();
        tracing::debug!(target: "agent_check", path = %path.display(), exists, "claude credential check");
        exists
    }
}
