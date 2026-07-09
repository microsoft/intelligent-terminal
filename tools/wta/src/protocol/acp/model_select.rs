//! Model-list extraction and model-switch dispatch across the two ways an
//! ACP agent can advertise its model selector.
//!
//! * **Legacy channel** — `NewSessionResponse.models` (a `SessionModelState`)
//!   plus the `session/set_model` method. Used by Copilot, Gemini, and the
//!   deprecated `@zed-industries/claude-code-acp` adapter.
//! * **Config-option channel** — `NewSessionResponse.config_options[]` with a
//!   `Select` entry whose category is `Model`, switched via
//!   `session/set_config_option`. Used by the renamed
//!   `@agentclientprotocol/claude-agent-acp` adapter (>= 0.24), which returns
//!   `Method not found` for `session/set_model`.
//!
//! A single `wta` process drives exactly one agent CLI, so the channel is
//! uniform for the whole process: [`models_from_new_session`] records it the
//! first time a `new_session` response is parsed and [`apply_session_model`]
//! reads it back when the user hot-swaps the model from a decoupled call site.

use std::sync::RwLock;

use agent_client_protocol as acp;

use crate::app::AcpModelInfo;

/// How the current agent expects a model switch to be delivered. Refreshed on
/// every `new_session` parse (see [`models_from_new_session`]) so an in-process
/// agent restart — or a session whose model selector advertises a different
/// config-option id — is always reflected, rather than frozen at first write.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ModelSwitchChannel {
    /// Legacy `session/set_model`.
    Legacy,
    /// `session/set_config_option` carrying this config id (e.g. `"model"`).
    Config { config_id: String },
}

/// The switch channel for this process's currently-connected agent. One `wta`
/// process drives one agent CLI, but the agent can restart in-process and a
/// later `new_session` may advertise a different channel/id — so this is a
/// mutable cell overwritten on every extraction, not a write-once latch.
static MODEL_SWITCH: RwLock<ModelSwitchChannel> = RwLock::new(ModelSwitchChannel::Legacy);

fn record_channel_config(config_id: &str) {
    *MODEL_SWITCH.write().unwrap() = ModelSwitchChannel::Config {
        config_id: config_id.to_string(),
    };
}

/// Extract the model list and current model id from a `new_session` response.
/// Schema 1.1 removed the legacy `NewSessionResponse.models` field, so this only
/// reads the `config_options` `Select` with `category == Model`: when present it
/// records the `Config` switch channel (otherwise the channel stays `Legacy`, its
/// default). Records the switch channel as a side effect so [`apply_session_model`]
/// later dispatches correctly.
pub(crate) fn models_from_new_session(
    resp: &acp::schema::v1::NewSessionResponse,
) -> (Vec<AcpModelInfo>, Option<String>) {
    if let Some(opts) = &resp.config_options {
        if let Some((config_id, models, current)) = model_option_from_config(opts) {
            record_channel_config(&config_id);
            return (models, current);
        }
    }

    (Vec::new(), None)
}

/// Find the model selector among a session's config options and flatten it
/// into `(config_id, models, current_model_id)`.
fn model_option_from_config(
    opts: &[acp::schema::v1::SessionConfigOption],
) -> Option<(String, Vec<AcpModelInfo>, Option<String>)> {
    // Pick the first option that is BOTH a model selector AND a Select. A
    // plain `find` on the category/id alone would bail out if a same-named
    // non-Select entry happened to come first, hiding a valid Select later in
    // the list.
    let (opt, sel) = opts.iter().find_map(|o| {
        let is_model = matches!(o.category, Some(acp::schema::v1::SessionConfigOptionCategory::Model))
            || o.id.0.as_ref() == "model";
        if !is_model {
            return None;
        }
        match &o.kind {
            acp::schema::v1::SessionConfigKind::Select(sel) => Some((o, sel)),
            _ => None,
        }
    })?;

    let flat: Vec<&acp::schema::v1::SessionConfigSelectOption> = match &sel.options {
        acp::schema::v1::SessionConfigSelectOptions::Ungrouped(v) => v.iter().collect(),
        acp::schema::v1::SessionConfigSelectOptions::Grouped(groups) => {
            groups.iter().flat_map(|g| g.options.iter()).collect()
        }
        _ => return None,
    };

    let models = flat
        .iter()
        .map(|o| AcpModelInfo {
            id: o.value.0.to_string(),
            name: o.name.clone(),
            description: o.description.clone(),
        })
        .collect();

    Some((
        opt.id.0.to_string(),
        models,
        Some(sel.current_value.0.to_string()),
    ))
}

/// Switch the model on a live session, routing to `session/set_model` or
/// `session/set_config_option` depending on the channel recorded by
/// [`models_from_new_session`]. On the config-option path, a `MethodNotFound`
/// response falls back to legacy `session/set_model`: some agents advertise a
/// config-option model selector for discovery yet only implement the legacy
/// switch method (the module docs call this out for Copilot/Gemini). Before
/// schema 1.1 removed `NewSessionResponse.models`, that field took priority and
/// this mismatch couldn't arise; now it can, so the fallback keeps model
/// switching working — and records `Legacy` so later switches skip the dead
/// config channel.
pub(crate) async fn apply_session_model(
    conn: &crate::protocol::acp::conn::ClientLink,
    session_id: acp::schema::v1::SessionId,
    model_id: String,
) -> acp::Result<()> {
    // Snapshot under the read lock and release it before the await — the lock
    // guard isn't Send and must not be held across the suspension point.
    let channel = MODEL_SWITCH.read().unwrap().clone();
    match channel {
        ModelSwitchChannel::Config { config_id } => {
            match conn
                .set_session_config_option(acp::schema::v1::SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    config_id,
                    model_id.clone(),
                ))
                .await
            {
                Ok(_) => Ok(()),
                Err(e) if e.code == acp::ErrorCode::MethodNotFound => {
                    *MODEL_SWITCH.write().unwrap() = ModelSwitchChannel::Legacy;
                    apply_legacy_set_model(conn, session_id, model_id).await
                }
                Err(e) => Err(e),
            }
        }
        ModelSwitchChannel::Legacy => apply_legacy_set_model(conn, session_id, model_id).await,
    }
}

/// Legacy `session/set_model` switch.
async fn apply_legacy_set_model(
    conn: &crate::protocol::acp::conn::ClientLink,
    session_id: acp::schema::v1::SessionId,
    model_id: String,
) -> acp::Result<()> {
    conn.set_session_model(crate::protocol::acp::conn::SetSessionModelRequest::new(
        session_id, model_id,
    ))
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    // `MODEL_SWITCH` is a process global; every test that reads or writes it
    // serializes on this lock so parallel test execution can't interleave.
    static SWITCH_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Real `session/new` wire shape from @agentclientprotocol/claude-agent-acp
    // (v0.44): no legacy `models` field — the model selector lives in
    // `configOptions` as a Select with category=model. Captured from
    // wta-acp-debug while validating issue #257.
    const CLAUDE_AGENT_ACP_NEW_SESSION: &str = r#"{
        "sessionId": "dac14599-682e-4a94-b48d-828101d22c05",
        "configOptions": [
            {
                "id": "mode", "name": "Mode", "category": "mode", "type": "select",
                "currentValue": "auto",
                "options": [{"value": "auto", "name": "Auto"}]
            },
            {
                "id": "model", "name": "Model", "description": "AI model to use",
                "category": "model", "type": "select", "currentValue": "default",
                "options": [
                    {"value": "default", "name": "Default (recommended)", "description": "currently Opus"},
                    {"value": "sonnet", "name": "Sonnet"},
                    {"value": "haiku", "name": "Haiku"}
                ]
            }
        ]
    }"#;

    // Legacy shape used by Copilot/Gemini and the deprecated
    // @zed-industries/claude-code-acp adapter.
    const LEGACY_NEW_SESSION: &str = r#"{
        "sessionId": "legacy-1",
        "models": {
            "availableModels": [
                {"modelId": "gpt-5.5", "name": "GPT-5.5"},
                {"modelId": "gpt-5.4", "name": "GPT-5.4"}
            ],
            "currentModelId": "gpt-5.5"
        }
    }"#;

    #[test]
    fn model_extraction_across_channels() {
        let _guard = SWITCH_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // Run sequentially in one test: the recorded switch channel is a
        // process-global, so splitting these into parallel #[test]s would race.

        // 1. New claude-agent-acp: models come from configOptions[category=model]
        //    and the switch channel flips to config-option.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(CLAUDE_AGENT_ACP_NEW_SESSION).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["default", "sonnet", "haiku"]);
        assert_eq!(current.as_deref(), Some("default"));
        // The model selector — not the "mode" selector — must win.
        assert_eq!(models[0].name, "Default (recommended)");
        assert_eq!(
            *MODEL_SWITCH.read().unwrap(),
            ModelSwitchChannel::Config {
                config_id: "model".to_string()
            }
        );

        // 2. Legacy `models` field was removed in schema 1.1 — a payload that
        //    only carries it (unknown to the deserializer) now yields no models;
        //    the config-option channel from step 1 stays recorded.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(LEGACY_NEW_SESSION).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        assert!(models.is_empty());
        assert_eq!(current, None);

        // 3. Neither channel present → empty list, no current model.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(r#"{"sessionId": "bare"}"#).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        assert!(models.is_empty());
        assert_eq!(current, None);

        // 4. Model selector identified by category alone (id != "model") is
        //    still found, and a preceding non-model Select is skipped — proves
        //    the find_map matches on the model predicate, not just position.
        let by_category = r#"{
            "sessionId": "cat-1",
            "configOptions": [
                {
                    "id": "mode", "name": "Mode", "category": "mode", "type": "select",
                    "currentValue": "auto", "options": [{"value": "auto", "name": "Auto"}]
                },
                {
                    "id": "llm", "name": "LLM", "category": "model", "type": "select",
                    "currentValue": "haiku",
                    "options": [{"value": "haiku", "name": "Haiku"}]
                }
            ]
        }"#;
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(by_category).expect("valid new_session");
        let (models, current) = models_from_new_session(&resp);
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["haiku"]);
        assert_eq!(current.as_deref(), Some("haiku"));
        // The channel id now UPDATES to this session's selector id ("llm"),
        // proving it is no longer frozen at the first-seen "model" id — the
        // exact regression the OnceLock version had.
        assert_eq!(
            *MODEL_SWITCH.read().unwrap(),
            ModelSwitchChannel::Config {
                config_id: "llm".to_string()
            }
        );
    }

    /// Wire a client `ClientLink` to a minimal agent that answers
    /// `session/set_config_option` as unimplemented (MethodNotFound when
    /// `config_method_not_found`, else a generic error) and the custom
    /// `session/set_model` with success — flipping `set_model_hit`. Lets the
    /// `apply_session_model` fallback be exercised end-to-end over real ACP.
    fn spawn_switch_mock(
        config_method_not_found: bool,
        set_model_hit: Arc<AtomicBool>,
    ) -> crate::protocol::acp::conn::ClientLink {
        use crate::protocol::acp::conn;
        let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
        let (cr, cw) = tokio::io::split(client_io);
        let (ar, aw) = tokio::io::split(agent_io);

        let client_builder = acp::Client
            .builder()
            .name("model-switch-test-client")
            .on_receive_request(
                |_req: acp::schema::v1::AgentRequest,
                 responder: acp::Responder<serde_json::Value>,
                 _cx| async move { responder.respond_with_error(acp::Error::method_not_found()) },
                acp::on_receive_request!(),
            )
            .on_receive_notification(
                |_n: acp::schema::v1::AgentNotification, _cx| async move { Ok(()) },
                acp::on_receive_notification!(),
            );
        let (client, client_io_fut) =
            conn::spawn_client(client_builder, conn::byte_streams(cw.compat_write(), cr.compat()));

        let agent_builder = acp::Agent
            .builder()
            .name("model-switch-test-agent")
            // Typed handler for the custom (schema-1.1-dropped) session/set_model.
            .on_receive_request(
                move |_req: conn::SetSessionModelRequest,
                      responder: acp::Responder<conn::SetSessionModelResponse>,
                      _cx| {
                    let hit = set_model_hit.clone();
                    async move {
                        hit.store(true, Ordering::SeqCst);
                        responder.respond(conn::SetSessionModelResponse::default())
                    }
                },
                acp::on_receive_request!(),
            )
            // Standard client->agent methods (notably session/set_config_option)
            // are answered as failures so the fallback path is exercised.
            .on_receive_request(
                move |_req: acp::schema::v1::ClientRequest,
                      responder: acp::Responder<serde_json::Value>,
                      _cx| async move {
                    let err = if config_method_not_found {
                        acp::Error::method_not_found()
                    } else {
                        acp::Error::internal_error()
                    };
                    responder.respond_with_error(err)
                },
                acp::on_receive_request!(),
            )
            .on_receive_notification(
                |_n: acp::schema::v1::ClientNotification, _cx| async move { Ok(()) },
                acp::on_receive_notification!(),
            );
        let (_agent, agent_io_fut) =
            conn::spawn_agent(agent_builder, conn::byte_streams(aw.compat_write(), ar.compat()));

        tokio::task::spawn_local(async move {
            let _ = client_io_fut.await;
        });
        tokio::task::spawn_local(async move {
            let _ = agent_io_fut.await;
        });
        client
    }

    #[test]
    fn config_channel_falls_back_to_set_model_on_method_not_found() {
        let _guard = SWITCH_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let hit = Arc::new(AtomicBool::new(false));
            let client = spawn_switch_mock(true, hit.clone());

            record_channel_config("model");
            let r = apply_session_model(&client, "s-fallback".into(), "haiku".to_string()).await;

            assert!(r.is_ok(), "fall back to set_model must succeed, got {r:?}");
            assert!(
                hit.load(Ordering::SeqCst),
                "set_model must be invoked as the fallback"
            );
            assert_eq!(
                *MODEL_SWITCH.read().unwrap(),
                ModelSwitchChannel::Legacy,
                "MethodNotFound on set_config_option must flip the channel to Legacy"
            );
        });
    }

    #[test]
    fn config_channel_does_not_fall_back_on_other_errors() {
        let _guard = SWITCH_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let hit = Arc::new(AtomicBool::new(false));
            let client = spawn_switch_mock(false, hit.clone());

            record_channel_config("model");
            let r = apply_session_model(&client, "s-other".into(), "haiku".to_string()).await;

            assert!(r.is_err(), "a non-MethodNotFound error must propagate");
            assert!(
                !hit.load(Ordering::SeqCst),
                "set_model must NOT be called for a non-MethodNotFound error"
            );
            assert!(
                matches!(*MODEL_SWITCH.read().unwrap(), ModelSwitchChannel::Config { .. }),
                "a non-MethodNotFound error must leave the channel on Config"
            );
        });
    }
}
