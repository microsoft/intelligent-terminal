//! Agent behavior abstraction (the "base agent" seam).
//!
//! This module introduces a single behavioral contract — the [`Agent`]
//! trait — that every supported agent CLI implements. It is the seam the
//! rest of the codebase should eventually call instead of the scattered
//! `match agent_id { "copilot" => …, "claude" => … }` blocks that currently
//! live in `agent_check.rs`, `app.rs`, `coordinator.rs`, and elsewhere.
//!
//! ## Why a trait on top of the existing registry?
//!
//! [`crate::agent_registry::AgentProfile`] is the *declarative* half of the
//! abstraction — flat data (flags, hints, resume support). That stays as a
//! data table; it is the right shape for "facts about an agent".
//!
//! What the table can't express is *behavior that genuinely diverges per
//! agent*: how to probe a credential, what the login subcommand is, whether a
//! BYOK / local-model configuration removes the need to authenticate at all.
//! Today that behavior is spread across `match agent_id` sites. The [`Agent`]
//! trait is where it belongs — one implementation per agent owns its own
//! divergent logic, and callers dispatch polymorphically.
//!
//! ## Two-axis auth model
//!
//! [`Agent::auth_needed`] encodes the distinction that Copilot CLI's BYOK
//! support (2026-04) made explicit: **model-inference auth** and
//! **platform-account auth** are independent. When the user has pointed the
//! agent at their own LLM provider (a local Ollama / Foundry Local endpoint,
//! or any BYOK endpoint), GitHub sign-in is *not required* to run — it only
//! unlocks optional platform features (`/delegate`, Code Search, …). Each
//! agent knows its own provider-configuration contract (the env vars it
//! reads), so the BYOK check lives in the per-agent implementation rather
//! than in a shared "LLM provider" abstraction.
//!
//! ## Migration status
//!
//! This is the first step: the trait and per-agent implementations exist and
//! are unit-tested, but they currently *delegate* to the existing
//! `agent_check` functions so behavior is byte-for-byte unchanged. Follow-up
//! changes will (a) route `agent_check`/`app.rs`/`coordinator.rs` decision
//! sites through this trait and (b) pull the divergent bodies down into the
//! impls. See the "match sites to eliminate" list in the crate refactor notes.

mod claude;
mod codex;
mod copilot;
mod gemini;

use crate::agent_registry::AgentProfile;
use crate::llm_provider::LlmProviderConfig;

/// Which inference backend a model is served by. Drives the `Cloud`/`Local`
/// tag in the `/model` picker and the switch semantics (crossing this boundary
/// requires reconfiguring the agent's provider env and respawning it, whereas
/// switching within `Cloud` is a live ACP `set_model`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelKind {
    /// Served by the agent's hosted/cloud backend (the ACP-advertised catalog).
    #[default]
    Cloud,
    /// Served by a local BYOK provider (Foundry Local / Ollama / custom endpoint).
    Local,
}

impl ModelKind {
    /// `true` for the default (cloud) kind — used to skip serializing the common
    /// case so existing `agent_status` consumers see no new field unless local.
    pub fn is_cloud(&self) -> bool {
        matches!(self, ModelKind::Cloud)
    }
}

/// One selectable model, mirroring `app::AcpModelInfo` but kept independent so
/// the `agent` module owns no dependency on `app`. Callers convert at the seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelEntry {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    /// Which backend serves this model — the picker tags it and switch logic
    /// keys on whether a pick crosses the cloud/local boundary.
    pub kind: ModelKind,
}

/// The model view the UI should present for an agent: the selectable list, the
/// active model id, and whether runtime switching is even possible.
///
/// Produced by [`Agent::resolve_models`]. For most agents this is exactly what
/// the ACP layer advertised (`new_session.models`); a provider that pins its
/// model out-of-band (Copilot BYOK's `COPILOT_MODEL`) replaces the list with
/// the real model and marks it non-switchable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    pub models: Vec<ModelEntry>,
    pub current_id: Option<String>,
    /// `false` when the active model is fixed for the life of the agent
    /// process and `session/set_model` cannot change it (e.g. BYOK pins the
    /// model via env at copilot startup). The UI disables switching.
    pub switchable: bool,
}

/// The current user's home directory (`%USERPROFILE%`), used by per-agent
/// native credential probes that look for config files. Empty when unset.
pub(crate) fn user_home() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
}

/// Behavioral contract implemented once per supported agent CLI.
///
/// Object-safe by design: every method takes `&self` and returns owned or
/// `'static` data, so callers can hold a `&'static dyn Agent` handed out by
/// [`agent_for_id`].
pub trait Agent {
    /// Canonical lowercase agent id (`"copilot"`, `"claude"`, …). Matches
    /// [`AgentProfile::id`].
    fn id(&self) -> &'static str;

    /// The declarative profile (flags, hints, capabilities) for this agent.
    fn profile(&self) -> &'static AgentProfile {
        crate::agent_registry::lookup_profile_by_id(self.id())
    }

    /// Whether this agent's CLI can be auto-installed by WTA (e.g. via
    /// winget). Replaces the scattered `id == "copilot"` install checks.
    fn can_auto_install(&self) -> bool {
        false
    }

    /// Whether WTA can drive this agent's sign-in *in-app* (e.g. Copilot's
    /// device-flow) versus requiring the user to authenticate externally and
    /// retry. Drives the SignIn-vs-Retry setup option and the sign-in screen
    /// subtitle. Defaults to `false` (external sign-in).
    fn drives_interactive_signin(&self) -> bool {
        false
    }

    /// Fast, synchronous check for a present platform credential
    /// (GitHub login, API key on disk, …). Exit 0 / file-exists style.
    ///
    /// The default runs the agent's declarative `auth_check_command` (if any)
    /// and otherwise falls back to [`Agent::probe_credential_native`]. This
    /// mirrors the historical two-strategy logic that used to live in
    /// `agent_check::has_credential`, now expressed as a polymorphic seam.
    fn probe_credential(&self) -> bool {
        // Strategy 1: declarative auth_check_command from the profile.
        if let Some(result) =
            crate::agent_check::run_auth_command(self.profile().auth_check_command)
        {
            return result;
        }
        // Strategy 2: agent-specific native check.
        self.probe_credential_native()
    }

    /// Agent-specific fast credential probe (cmdkey query, config-file
    /// existence, API-key env var, …). Overridden per agent; the default is
    /// "no credential found".
    fn probe_credential_native(&self) -> bool {
        false
    }

    /// The CLI subcommand that starts this agent's interactive sign-in
    /// (appended after the resolved executable path by
    /// [`crate::agent_check::build_login_cmd`]). Defaults to `"login"`;
    /// agents whose CLI uses a different verb override this.
    fn login_subcommand(&self) -> &'static str {
        "login"
    }

    /// Whether WTA must gate on platform authentication before connecting.
    ///
    /// This is the key new seam. The default is the historical behavior —
    /// "auth is needed iff no credential is present". Agents that support a
    /// BYOK / local-model provider override this to return `false` when such
    /// a provider is configured, because the agent does not require a
    /// platform sign-in to run inference against the user's own model.
    fn auth_needed(&self) -> bool {
        !self.probe_credential()
    }

    /// Resolve the model catalog the UI should present, given what the ACP
    /// layer advertised in `new_session` (`acp`). The default passes the ACP
    /// list through unchanged and switchable — the agent's advertised models
    /// are authoritative. Agents whose model can be pinned out-of-band (e.g.
    /// Copilot BYOK via `COPILOT_MODEL`) override this to report the real
    /// model and mark it non-switchable, because the ACP selector is decoupled
    /// from the actual inference routing in that mode.
    fn resolve_models(&self, acp: ModelCatalog) -> ModelCatalog {
        acp
    }

    /// Whether this agent has a wired-up BYOK (bring-your-own-LLM) env
    /// contract — i.e. [`Agent::byok_env`] can translate an
    /// [`LlmProviderConfig`] into env vars its CLI understands. Defaults to
    /// `false`; only agents that document a custom-provider env contract
    /// override it. Used to decide whether BYOK config is meaningful for this
    /// agent at all.
    fn supports_byok(&self) -> bool {
        false
    }

    /// Translate a generic [`LlmProviderConfig`] into the concrete environment
    /// variables this agent's CLI reads to route inference at a BYOK endpoint.
    ///
    /// This is the per-agent half of the LLM-provider abstraction: the
    /// `llm_provider` module owns the *generic* backend description, and each
    /// agent owns the mapping onto its own env-var *names* (copilot:
    /// `COPILOT_PROVIDER_*`; a future claude/gemini: their own). The returned
    /// pairs are injected onto the spawned agent-CLI child in
    /// [`crate::protocol::acp::spawn::spawn_agent_process`].
    ///
    /// The default returns no pairs — an agent without a BYOK contract gets no
    /// injected provider env. Callers should only invoke this when
    /// [`LlmProviderConfig::is_active`] is true.
    fn byok_env(&self, _cfg: &LlmProviderConfig) -> Vec<(String, String)> {
        Vec::new()
    }

    /// The full set of environment variable *names* this agent reads for BYOK.
    ///
    /// Used to force an agent back to its hosted/cloud backend on respawn: when
    /// the user switches a pinned local model to a cloud one, the spawner must
    /// *remove* these vars from the child so an ambient (e.g. machine-scoped)
    /// BYOK config can't keep routing to the local endpoint. The default is
    /// empty (no BYOK contract). Should be a superset of every key
    /// [`Agent::byok_env`] can emit.
    fn byok_env_keys(&self) -> Vec<&'static str> {
        Vec::new()
    }
}

/// Whether WTA must gate on platform authentication before connecting the
/// given agent.
///
/// This is the call decision sites should use instead of the bare
/// `!agent_check::has_credential(id)` they historically used. For a known
/// agent it dispatches to [`Agent::auth_needed`] (which lets Copilot drop the
/// gate when a BYOK / local-model provider is configured). For an
/// unrecognized id (e.g. a `custom:` agent) it falls back to the legacy
/// credential check so existing behavior is preserved.
pub fn auth_needed_for(id: &str) -> bool {
    match agent_for_id(id) {
        Some(agent) => agent.auth_needed(),
        None => !crate::agent_check::has_credential(id),
    }
}

/// Return the `'static` [`Agent`] implementation for a canonical agent id.
///
/// Returns `None` for an unrecognized agent so callers can decide how to
/// treat it (the registry's `DEFAULT_PROFILE` fallback is intentionally not
/// mirrored here — an unknown id has no behavior to dispatch).
pub fn agent_for_id(id: &str) -> Option<&'static dyn Agent> {
    match id.to_ascii_lowercase().as_str() {
        "copilot" => Some(&copilot::CopilotAgent),
        "claude" => Some(&claude::ClaudeAgent),
        "codex" => Some(&codex::CodexAgent),
        "gemini" => Some(&gemini::GeminiAgent),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_for_id_resolves_known_agents_and_is_case_insensitive() {
        for id in ["copilot", "claude", "codex", "gemini"] {
            let agent = agent_for_id(id).unwrap_or_else(|| panic!("missing impl for {id}"));
            assert_eq!(agent.id(), id, "id() must echo the canonical id");
            assert_eq!(
                agent.profile().id,
                id,
                "profile() must resolve to the matching registry entry"
            );
        }
        assert!(agent_for_id("COPILOT").is_some(), "lookup is case-insensitive");
        assert!(agent_for_id("nonexistent").is_none());
    }

    #[test]
    fn only_copilot_auto_installs() {
        assert!(agent_for_id("copilot").unwrap().can_auto_install());
        for id in ["claude", "codex", "gemini"] {
            assert!(
                !agent_for_id(id).unwrap().can_auto_install(),
                "{id} should not be auto-installable"
            );
        }
    }

    #[test]
    fn auth_needed_for_unknown_agent_falls_back_to_credential_check() {
        // An unrecognized id has no `Agent` impl; the helper must fall back to
        // the legacy credential check. A made-up agent has no credential, so
        // auth is required (matching the pre-seam behavior).
        assert!(agent_for_id("custom:does-not-exist").is_none());
        assert!(
            auth_needed_for("custom:does-not-exist"),
            "unknown agents fall back to !has_credential, which is true here"
        );
    }

    #[test]
    fn login_subcommands_match_each_cli() {
        // Locks the per-agent login verbs migrated off the old
        // `match agent_id` in agent_check::build_login_cmd.
        assert_eq!(agent_for_id("copilot").unwrap().login_subcommand(), "login");
        assert_eq!(agent_for_id("claude").unwrap().login_subcommand(), "login");
        assert_eq!(agent_for_id("codex").unwrap().login_subcommand(), "auth");
        assert_eq!(agent_for_id("gemini").unwrap().login_subcommand(), "auth login");
    }
}
