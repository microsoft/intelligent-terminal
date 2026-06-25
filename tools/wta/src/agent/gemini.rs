//! Gemini agent implementation.
//!
//! Thin for now — Gemini inherits the default [`Agent`] behavior. Override
//! per-agent methods here as Gemini-specific logic is migrated off the
//! scattered `match agent_id` sites.

use super::Agent;

pub struct GeminiAgent;

impl Agent for GeminiAgent {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn login_subcommand(&self) -> &'static str {
        "auth login"
    }

    fn probe_credential_native(&self) -> bool {
        // GEMINI_API_KEY / GOOGLE_API_KEY env var, or an OAuth token cached in
        // ~/.gemini/.
        std::env::var("GEMINI_API_KEY").is_ok()
            || std::env::var("GOOGLE_API_KEY").is_ok()
            || super::user_home().join(".gemini").exists()
    }
}
