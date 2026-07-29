//! Detection and application of an agent-native "allow all" permission
//! config option, discovered from `NewSessionResponse.config_options` the
//! same way [`crate::protocol::acp::model_select`] discovers the model
//! selector.
//!
//! Some agents (confirmed for Copilot CLI's ACP server, schema >= 1.1)
//! advertise a `session/set_config_option`-switchable, **per-session**
//! permission config (category `permissions`, id `allow_all`, a boolean-ish
//! `on`/`off` select) that — once flipped to `on` for a given `session_id` —
//! makes the agent stop sending `session/request_permission` for that
//! session entirely. This is the agent *itself* deciding to skip approval,
//! rather than WTA intercepting and auto-answering every request as
//! `client.rs`'s yolo-mode `request_permission` handling does.
//!
//! When available, WTA prefers this native channel for yolo mode (global
//! toggle and per-session `/yolo` alike): it is properly session-scoped by
//! the agent itself (no risk of leaking `allow_always` to an unrelated tab
//! sharing the same agent process — see the multiplexing note in
//! `master/mod.rs`), and it avoids the tool-call being paused, however
//! briefly, on a request/response round-trip through master.
//!
//! Agents that don't advertise this config option are unaffected: WTA's
//! existing client-side `request_permission` auto-approve in `client.rs`
//! keeps working as the fallback, unconditionally, for every agent.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};

use agent_client_protocol as acp;

/// The `session/set_config_option` id for the currently-connected agent's
/// native allow-all permission config, if it advertised one in the most
/// recent `new_session` response. `None` means either no such option was
/// seen yet, or this agent doesn't have one — in both cases callers must
/// fall back to client-side `request_permission` interception.
///
/// One `wta` process drives one agent CLI (see `model_select`'s module
/// docs for the same invariant), so — like `ModelSwitchChannel` — this is a
/// process-global refreshed on every `new_session` parse rather than a
/// write-once latch.
static ALLOW_ALL_CONFIG_ID: RwLock<Option<String>> = RwLock::new(None);

/// Find the native allow-all permission selector among a session's config
/// options, matching on category `permissions` OR id `allow_all` (mirrors
/// `model_select::model_option_from_config`'s dual match, since some agents
/// may omit the category and only set the id, or vice versa).
///
/// Like `model_option_from_config`, requires the matched entry to also be a
/// `Select` (the only kind `session/set_config_option` can flip to "on"/
/// "off" here) — a plain match on category/id alone would stop at the first
/// same-named non-Select entry, hiding a valid Select later in the list.
fn allow_all_option_id(opts: &[acp::schema::v1::SessionConfigOption]) -> Option<String> {
    opts.iter()
        .find(|o| {
            let is_permissions = matches!(
                &o.category,
                Some(acp::schema::v1::SessionConfigOptionCategory::Other(cat)) if cat == "permissions"
            ) || o.id.0.as_ref() == "allow_all";
            is_permissions && matches!(o.kind, acp::schema::v1::SessionConfigKind::Select(_))
        })
        .map(|o| o.id.0.to_string())
}

/// Record whether the current agent advertises a native allow-all config
/// option, from a `new_session` response. Called alongside
/// `model_select::models_from_new_session` at every session-creation call
/// site in `client.rs`.
pub(crate) fn record_from_new_session(resp: &acp::schema::v1::NewSessionResponse) {
    let found = resp
        .config_options
        .as_deref()
        .and_then(allow_all_option_id);
    *ALLOW_ALL_CONFIG_ID.write().unwrap() = found;
}

/// The recorded native allow-all config id, if the current agent has one.
pub(crate) fn native_allow_all_config_id() -> Option<String> {
    ALLOW_ALL_CONFIG_ID.read().unwrap().clone()
}

/// This session's yolo state, mirrored from `App`/`ClientState` so the
/// session-creation call sites in `client.rs` (which don't otherwise carry
/// `global_auto_approve_tools`/`yolo_sessions`) can decide whether to
/// proactively apply the native allow-all channel to a *brand-new* session
/// as soon as it's created, without threading two extra parameters through
/// every `new_session` call site in the file.
///
/// `(global_auto_approve, yolo_sessions)` — `global_auto_approve` is fixed
/// for the helper's lifetime (set once from the `--auto-approve-tools`
/// flag), `yolo_sessions` is the same shared set `/yolo` mutates. Seeded by
/// [`init_yolo_state`] once per `run_acp_client_over_pipe` call (including
/// reconnects — harmless, since both values are stable/the same `Arc`
/// across reconnects of one helper process).
static YOLO_STATE: RwLock<(bool, Option<Arc<Mutex<HashSet<String>>>>)> = RwLock::new((false, None));

/// Seed the process-global yolo state mirror. Called once from
/// `run_acp_client_over_pipe` alongside `ClientState` construction, which
/// receives the same two values.
pub(crate) fn init_yolo_state(global_auto_approve: bool, yolo_sessions: Arc<Mutex<HashSet<String>>>) {
    *YOLO_STATE.write().unwrap() = (global_auto_approve, Some(yolo_sessions));
}

/// Whether `session_id` should run in yolo mode — either the global toggle
/// is on, or this specific session ran `/yolo`. Mirrors the same check in
/// `client.rs`'s `request_permission` handler; kept here too so the
/// session-creation call sites can decide whether it's worth attempting the
/// native allow-all apply at all.
pub(crate) fn is_yolo_session(session_id: &str) -> bool {
    let guard = YOLO_STATE.read().unwrap();
    guard.0
        || guard
            .1
            .as_ref()
            .map(|s| s.lock().unwrap().contains(session_id))
            .unwrap_or(false)
}

/// Switch the recorded native allow-all config to `on` for one session, via
/// `session/set_config_option`. Returns `Ok(false)` (not an error) when the
/// current agent doesn't advertise the config — callers rely on the
/// client-side `request_permission` auto-approve fallback in that case
/// instead. Returns `Ok(true)` when the native call was actually made and
/// succeeded.
pub(crate) async fn apply_native_allow_all(
    conn: &crate::protocol::acp::conn::ClientLink,
    session_id: acp::schema::v1::SessionId,
) -> acp::Result<bool> {
    let Some(config_id) = native_allow_all_config_id() else {
        return Ok(false);
    };
    conn.set_session_config_option(
        acp::schema::v1::SetSessionConfigOptionRequest::new(session_id, config_id, "on"),
    )
    .await
    .map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Process-global like `model_select::MODEL_SWITCH` — serialize tests
    // that touch it.
    static ALLOW_ALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    #[test]
    fn records_allow_all_config_id_when_present() {
        let _guard = ALLOW_ALL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(COPILOT_NEW_SESSION).expect("valid new_session");
        record_from_new_session(&resp);
        assert_eq!(native_allow_all_config_id().as_deref(), Some("allow_all"));
    }

    #[test]
    fn clears_recorded_id_when_agent_has_no_native_channel() {
        let _guard = ALLOW_ALL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(COPILOT_NEW_SESSION).expect("valid new_session");
        record_from_new_session(&resp);
        assert!(native_allow_all_config_id().is_some());

        // A later session on an agent without the option (or the same
        // agent's response simply omitting it) must clear the stale id —
        // otherwise a stale config_id could be sent to an agent that no
        // longer advertises it, mirroring the "refreshed, not a latch"
        // invariant `model_select::MODEL_SWITCH` documents.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(NO_ALLOW_ALL_NEW_SESSION).expect("valid new_session");
        record_from_new_session(&resp);
        assert_eq!(native_allow_all_config_id(), None);
    }

    #[test]
    fn is_yolo_session_reflects_global_and_per_session_state() {
        let _guard = ALLOW_ALL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sessions = Arc::new(Mutex::new(HashSet::new()));
        init_yolo_state(false, sessions.clone());
        assert!(!is_yolo_session("s-a"));

        sessions.lock().unwrap().insert("s-a".to_string());
        assert!(is_yolo_session("s-a"), "per-session /yolo membership must be honored");
        assert!(!is_yolo_session("s-b"), "an unrelated session must not be affected");

        init_yolo_state(true, sessions.clone());
        assert!(is_yolo_session("s-b"), "global toggle must cover every session");
    }

    #[test]
    fn skips_non_select_entry_and_finds_later_valid_select() {
        let _guard = ALLOW_ALL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        record_from_new_session(&resp);
        assert_eq!(native_allow_all_config_id().as_deref(), Some("allow_all"));
    }

    #[test]
    fn matched_by_id_alone_without_category() {
        let _guard = ALLOW_ALL_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
        record_from_new_session(&resp);
        assert_eq!(native_allow_all_config_id().as_deref(), Some("allow_all"));
    }
}
