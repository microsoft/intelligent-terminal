//! Detection and application of Copilot's native "allow all" permission
//! config option from `NewSessionResponse.config_options`.
//!
//! Copilot CLI's ACP server (schema >= 1.1) advertises a
//! `session/set_config_option`-switchable, **per-session**
//! permission config (category `permissions`, id `allow_all`, a boolean-ish
//! `on`/`off` select) that — once flipped to `on` for a given `session_id` —
//! makes the agent stop sending `session/request_permission` for that
//! session entirely. This is the agent *itself* deciding to skip approval,
//! rather than WTA intercepting and auto-answering every request as
//! `client.rs`'s yolo-mode `request_permission` handling does.
//!
//! For Copilot only, WTA prefers this native channel for yolo mode (global
//! toggle and per-session `/yolo` alike): it is properly session-scoped by
//! the agent itself (no risk of leaking `allow_always` to an unrelated tab
//! sharing the same agent process — see the multiplexing note in
//! `master/mod.rs`), and it avoids the tool-call being paused, however
//! briefly, on a request/response round-trip through master.
//!
//! Every other agent, including a custom agent that advertises a similarly
//! named option, uses WTA's existing client-side `request_permission`
//! auto-approve fallback in `client.rs`. This fail-closed allowlist avoids
//! assigning Copilot-specific semantics to an unrelated agent's config.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use agent_client_protocol as acp;

/// The `session/set_config_option` id for the currently-connected agent's
/// native allow-all permission config, if it advertised one in the most
/// recent `new_session` response. `None` means either no such option was
/// seen yet, or this agent doesn't have one — in both cases callers must
/// fall back to client-side `request_permission` interception.
///
/// A master may host multiple agent CLIs, but each helper connection is bound
/// to one selected Agent instance. This connection-owned state is refreshed
/// on every `new_session` response rather than used as a write-once latch.
pub(crate) struct PermissionSelectState {
    allow_all_config_id: RwLock<Option<String>>,
    copilot_native_allow_all: AtomicBool,
}

impl PermissionSelectState {
    pub(crate) fn new() -> Self {
        Self {
            allow_all_config_id: RwLock::new(None),
            copilot_native_allow_all: AtomicBool::new(false),
        }
    }

    pub(crate) fn set_resolved_agent_id(&self, agent_id: Option<&str>) {
        let copilot = agent_id == Some(crate::agent_registry::COPILOT_AGENT_ID);
        self.copilot_native_allow_all
            .store(copilot, Ordering::Release);
        if !copilot {
            *self.allow_all_config_id.write().unwrap() = None;
        }
    }

    pub(crate) fn record_from_new_session(
        &self,
        resp: &acp::schema::v1::NewSessionResponse,
    ) {
        self.record_config_options(resp.config_options.as_deref());
    }

    pub(crate) fn record_from_load_session(
        &self,
        resp: &acp::schema::v1::LoadSessionResponse,
    ) {
        self.record_config_options(resp.config_options.as_deref());
    }

    pub(crate) fn record_config_options(
        &self,
        config_options: Option<&[acp::schema::v1::SessionConfigOption]>,
    ) {
        let found = self
            .copilot_native_allow_all
            .load(Ordering::Acquire)
            .then(|| config_options.and_then(allow_all_option_id))
            .flatten();
        *self.allow_all_config_id.write().unwrap() = found;
    }

    pub(crate) fn copilot_requires_native_disable(&self) -> bool {
        self.copilot_native_allow_all.load(Ordering::Acquire)
    }

    fn native_allow_all_config_id(&self) -> Option<String> {
        self.allow_all_config_id.read().unwrap().clone()
    }

    pub(crate) async fn set_native_allow_all(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        session_id: acp::schema::v1::SessionId,
        enabled: bool,
    ) -> acp::Result<bool> {
        if !self.copilot_native_allow_all.load(Ordering::Acquire) {
            return Ok(false);
        }
        let Some(config_id) = self.native_allow_all_config_id() else {
            return Ok(false);
        };
        let value = if enabled { "on" } else { "off" };
        conn.set_session_config_option(
            acp::schema::v1::SetSessionConfigOptionRequest::new(session_id, config_id, value),
        )
        .await
        .map(|_| true)
    }
}

/// Match only the exact Copilot contract verified on the wire. Requiring the
/// canonical id, category, Select kind, and both values prevents a partial or
/// similarly named option from being treated as an unrestricted permission
/// switch.
fn allow_all_option_id(opts: &[acp::schema::v1::SessionConfigOption]) -> Option<String> {
    opts.iter().find_map(|option| {
        if option.id.0.as_ref() != "allow_all"
            || !matches!(
                &option.category,
                Some(acp::schema::v1::SessionConfigOptionCategory::Other(category))
                    if category == "permissions"
            )
        {
            return None;
        }
        let acp::schema::v1::SessionConfigKind::Select(select) = &option.kind else {
            return None;
        };
        let values: Vec<&str> = match &select.options {
            acp::schema::v1::SessionConfigSelectOptions::Ungrouped(options) => {
                options.iter().map(|entry| entry.value.0.as_ref()).collect()
            }
            acp::schema::v1::SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|group| group.options.iter())
                .map(|entry| entry.value.0.as_ref())
                .collect(),
            _ => return None,
        };
        (values.contains(&"on") && values.contains(&"off"))
            .then(|| option.id.0.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real wire shape captured from `copilot --acp --stdio`'s `session/new`
    // response (verified live: setting this config option to "on" then
    // prompting a tool call produced zero `request_permission` round-trips).
    const COPILOT_NEW_SESSION: &str = r#"{
        "sessionId": "s-1",
        "configOptions": [
            {
                "id": "mode", "name": "Mode", "category": "mode", "type": "select",
                "currentValue": "agent", "options": [{"value": "agent", "name": "Agent"}]
            },
            {
                "id": "allow_all", "name": "Allow All", "category": "permissions", "type": "select",
                "currentValue": "off",
                "options": [
                    {"value": "on", "name": "On"},
                    {"value": "off", "name": "Off"}
                ]
            }
        ]
    }"#;

    const NO_ALLOW_ALL_NEW_SESSION: &str = r#"{
        "sessionId": "s-2",
        "configOptions": [
            {
                "id": "model", "name": "Model", "category": "model", "type": "select",
                "currentValue": "default", "options": [{"value": "default", "name": "Default"}]
            }
        ]
    }"#;

    const COPILOT_LOAD_SESSION: &str = r#"{
        "configOptions": [
            {
                "id": "allow_all", "name": "Allow All", "category": "permissions", "type": "select",
                "currentValue": "on",
                "options": [
                    {"value": "on", "name": "On"},
                    {"value": "off", "name": "Off"}
                ]
            }
        ]
    }"#;

    #[test]
    fn records_allow_all_config_id_when_present() {
        let state = PermissionSelectState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(COPILOT_NEW_SESSION).expect("valid new_session");
        state.record_from_new_session(&resp);
        assert_eq!(state.native_allow_all_config_id().as_deref(), Some("allow_all"));
    }

    #[test]
    fn clears_recorded_id_when_agent_has_no_native_channel() {
        let state = PermissionSelectState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(COPILOT_NEW_SESSION).expect("valid new_session");
        state.record_from_new_session(&resp);
        assert!(state.native_allow_all_config_id().is_some());

        // A later session on an agent without the option (or the same
        // agent's response simply omitting it) must clear the stale id —
        // otherwise a stale config_id could be sent to an agent that no
        // longer advertises it, mirroring the "refreshed, not a latch"
        // invariant `model_select::MODEL_SWITCH` documents.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(NO_ALLOW_ALL_NEW_SESSION).expect("valid new_session");
        state.record_from_new_session(&resp);
        assert_eq!(state.native_allow_all_config_id(), None);
    }

    #[test]
    fn records_allow_all_config_id_from_loaded_session() {
        let state = PermissionSelectState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let response: acp::schema::v1::LoadSessionResponse =
            serde_json::from_str(COPILOT_LOAD_SESSION).expect("valid load_session");

        state.record_from_load_session(&response);

        assert_eq!(
            state.native_allow_all_config_id().as_deref(),
            Some("allow_all")
        );
    }

    #[test]
    fn skips_non_select_entry_and_finds_later_valid_select() {
        let state = PermissionSelectState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        // A non-Select entry that happens to match id/category ("allow_all"
        // as a plain boolean toggle, no `options`/`type: select`) must not
        // shadow a later, genuinely-Select entry with the same id — mirrors
        // `model_select::model_option_from_config`'s guard against the same
        // pitfall.
        let resp: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "s-4",
                "configOptions": [
                    {"id": "allow_all", "name": "Allow All (legacy boolean)", "category": "permissions", "type": "boolean", "currentValue": false},
                    {"id": "allow_all", "name": "Allow All", "category": "permissions", "type": "select", "currentValue": "off",
                     "options": [{"value": "on", "name": "On"}, {"value": "off", "name": "Off"}]}
                ]
            }"#,
        )
        .expect("valid new_session");
        state.record_from_new_session(&resp);
        assert_eq!(state.native_allow_all_config_id().as_deref(), Some("allow_all"));
    }

    #[test]
    fn rejects_matching_option_without_copilot_category() {
        let state = PermissionSelectState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let resp: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "s-3",
                "configOptions": [
                    {
                        "id": "allow_all", "name": "Allow All", "type": "select",
                        "currentValue": "off",
                        "options": [{"value": "on", "name": "On"}, {"value": "off", "name": "Off"}]
                    }
                ]
            }"#,
        )
        .expect("valid new_session");
        state.record_from_new_session(&resp);
        assert_eq!(state.native_allow_all_config_id(), None);
    }

    #[test]
    fn non_copilot_agent_cannot_enable_native_allow_all() {
        let state = PermissionSelectState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(COPILOT_NEW_SESSION).expect("valid new_session");

        state.record_from_new_session(&resp);

        assert_eq!(state.native_allow_all_config_id(), None);
    }

    #[test]
    fn rejects_copilot_selector_without_both_verified_values() {
        let state = PermissionSelectState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let resp: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "s-5",
                "configOptions": [{
                    "id": "allow_all",
                    "name": "Allow All",
                    "category": "permissions",
                    "type": "select",
                    "currentValue": "on",
                    "options": [{"value": "on", "name": "On"}]
                }]
            }"#,
        )
        .expect("valid new_session");

        state.record_from_new_session(&resp);

        assert_eq!(state.native_allow_all_config_id(), None);
    }
}
