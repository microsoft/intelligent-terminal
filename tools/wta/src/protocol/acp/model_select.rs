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
//! Model extraction is pure. Callers retain the returned switch channel next
//! to the connection it describes and pass it back to [`apply_session_model`].
//! This is important for wta-master, which pools multiple unrelated agent CLIs
//! in one process.

use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::app_contracts::AcpModelInfo;

pub(crate) const WTA_CLOUD_CATALOG_AVAILABLE: &str = "_intellterm.wta/cloud_catalog_available";

/// How an agent expects a model switch to be delivered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum ModelSwitchChannel {
    /// Legacy `session/set_model`.
    #[default]
    Legacy,
    /// `session/set_config_option` carrying this config id (e.g. `"model"`).
    Config { config_id: String },
}

/// Model catalog and switching metadata advertised by one `session/new`.
#[derive(Clone, Debug, Default)]
pub(crate) struct NewSessionModelState {
    pub(crate) available_models: Vec<AcpModelInfo>,
    pub(crate) current_model_id: Option<String>,
    pub(crate) switch_channel: ModelSwitchChannel,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CloudModelCatalogMetadata {
    pub(crate) models: Vec<AcpModelInfo>,
    pub(crate) source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CloudCatalogNotification {
    models: Vec<AcpModelInfo>,
    source: String,
}

pub(crate) fn inject_wta_cloud_catalog(
    meta: &mut Option<acp::schema::v1::Meta>,
    models: &[AcpModelInfo],
    source: &str,
) -> Result<(), serde_json::Error> {
    if models.is_empty() {
        return Ok(());
    }
    crate::session_registry::inject_wta_meta(
        meta,
        &crate::session_registry::WtaMeta {
            cloud_models: Some(serde_json::to_string(models)?),
            cloud_models_source: Some(source.to_string()),
            ..Default::default()
        },
    );
    Ok(())
}

pub(crate) fn extract_wta_cloud_catalog(
    meta: &mut Option<acp::schema::v1::Meta>,
) -> CloudModelCatalogMetadata {
    let wta = crate::session_registry::extract_wta_meta(meta);
    let models = wta
        .cloud_models
        .as_deref()
        .and_then(|raw| match serde_json::from_str(raw) {
            Ok(models) => Some(models),
            Err(error) => {
                tracing::warn!(
                    target: "cloud_models",
                    %error,
                    source = ?wta.cloud_models_source,
                    "invalid cloud model catalog in private WTA metadata"
                );
                None
            }
        })
        .unwrap_or_default();
    CloudModelCatalogMetadata {
        models,
        source: wta.cloud_models_source,
    }
}

pub(crate) fn build_wta_cloud_catalog_notification(
    models: &[AcpModelInfo],
    source: &str,
) -> acp::schema::v1::ExtNotification {
    let params = CloudCatalogNotification {
        models: models.to_vec(),
        source: source.to_string(),
    };
    let json = serde_json::to_string(&params)
        .expect("CloudCatalogNotification serialization is infallible for owned data");
    let raw = serde_json::value::RawValue::from_string(json)
        .expect("serde_json::to_string always produces valid JSON");
    acp::schema::v1::ExtNotification::new(WTA_CLOUD_CATALOG_AVAILABLE, Arc::from(raw))
}

pub(crate) fn parse_wta_cloud_catalog_notification(
    notification: &acp::schema::v1::ExtNotification,
) -> Option<Result<CloudModelCatalogMetadata, serde_json::Error>> {
    if !crate::session_registry::ext_method_matches(
        &notification.method,
        WTA_CLOUD_CATALOG_AVAILABLE,
    ) {
        return None;
    }

    Some(
        serde_json::from_str::<CloudCatalogNotification>(notification.params.get()).map(
            |catalog| CloudModelCatalogMetadata {
                models: catalog.models,
                source: Some(catalog.source),
            },
        ),
    )
}

/// Extract the model list and current model id from a `new_session` response.
/// Schema 1.1 removed the legacy `NewSessionResponse.models` field, so this only
/// reads the `config_options` `Select` with `category == Model`. A response with
/// no selector uses the legacy channel without inventing catalog metadata.
pub(crate) fn models_from_new_session(
    resp: &acp::schema::v1::NewSessionResponse,
) -> NewSessionModelState {
    model_state_from_config_options(resp.config_options.as_deref()).unwrap_or_default()
}

/// Extract model state from `session/load`. Unlike `session/new`, an absent
/// selector must not reset the retained switch channel: direct resume may be
/// rebinding an already-loaded session whose selector was discovered earlier.
pub(crate) fn models_from_load_session(
    resp: &acp::schema::v1::LoadSessionResponse,
) -> Option<NewSessionModelState> {
    model_state_from_config_options(resp.config_options.as_deref())
}

fn model_state_from_config_options(
    options: Option<&[acp::schema::v1::SessionConfigOption]>,
) -> Option<NewSessionModelState> {
    let (config_id, available_models, current_model_id) = model_option_from_config(options?)?;
    Some(NewSessionModelState {
        available_models,
        current_model_id,
        switch_channel: ModelSwitchChannel::Config { config_id },
    })
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
        let is_model = matches!(
            o.category,
            Some(acp::schema::v1::SessionConfigOptionCategory::Model)
        ) || o.id.0.as_ref() == "model";
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
/// `session/set_config_option` using the channel retained by the caller. On the
/// config-option path, a `MethodNotFound` response falls back to legacy
/// `session/set_model`: some agents advertise a config-option model selector
/// for discovery yet only implement the legacy switch method. The fallback
/// also downgrades the caller's channel so later switches skip the dead config
/// route.
pub(crate) async fn apply_session_model(
    conn: &crate::protocol::acp::conn::ClientLink,
    channel: &mut ModelSwitchChannel,
    session_id: acp::schema::v1::SessionId,
    model_id: String,
) -> acp::Result<()> {
    match channel.clone() {
        ModelSwitchChannel::Config { config_id } => {
            match conn
                .set_session_config_option(acp::schema::v1::SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    config_id,
                    model_id.as_str(),
                ))
                .await
            {
                Ok(_) => Ok(()),
                Err(e) if e.code == acp::ErrorCode::MethodNotFound => {
                    *channel = ModelSwitchChannel::Legacy;
                    request_session_model(conn, session_id, model_id).await
                }
                Err(e) => Err(e),
            }
        }
        ModelSwitchChannel::Legacy => request_session_model(conn, session_id, model_id).await,
    }
}

/// Send the schema-1.1-dropped `session/set_model` request. Helper connections
/// use this to let wta-master route through the bound AgentCli's retained
/// switch channel; direct single-agent clients use [`apply_session_model`].
pub(crate) async fn request_session_model(
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
        // 1. New claude-agent-acp: models come from configOptions[category=model]
        //    and extraction returns the config-option switch channel.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(CLAUDE_AGENT_ACP_NEW_SESSION).expect("valid new_session");
        let state = models_from_new_session(&resp);
        let ids: Vec<&str> = state
            .available_models
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, vec!["default", "sonnet", "haiku"]);
        assert_eq!(state.current_model_id.as_deref(), Some("default"));
        // The model selector — not the "mode" selector — must win.
        assert_eq!(state.available_models[0].name, "Default (recommended)");
        assert_eq!(
            state.switch_channel,
            ModelSwitchChannel::Config {
                config_id: "model".to_string()
            }
        );

        // 2. Legacy `models` field was removed in schema 1.1 — a payload that
        //    only carries it (unknown to the deserializer) now yields no models
        //    and explicitly selects the legacy switch channel.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(LEGACY_NEW_SESSION).expect("valid new_session");
        let state = models_from_new_session(&resp);
        assert!(state.available_models.is_empty());
        assert_eq!(state.current_model_id, None);
        assert_eq!(state.switch_channel, ModelSwitchChannel::Legacy);

        // 3. Neither channel present → empty list, no current model, and no
        //    fabricated config_options/currentValue metadata.
        let resp: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(r#"{"sessionId": "bare"}"#).expect("valid new_session");
        let state = models_from_new_session(&resp);
        assert!(state.available_models.is_empty());
        assert_eq!(state.current_model_id, None);
        assert_eq!(state.switch_channel, ModelSwitchChannel::Legacy);
        assert!(resp.config_options.is_none());

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
        let state = models_from_new_session(&resp);
        let ids: Vec<&str> = state
            .available_models
            .iter()
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(ids, vec!["haiku"]);
        assert_eq!(state.current_model_id.as_deref(), Some("haiku"));
        assert_eq!(
            state.switch_channel,
            ModelSwitchChannel::Config {
                config_id: "llm".to_string()
            }
        );
    }

    #[test]
    fn load_session_extracts_config_option_model_state() {
        let response: acp::schema::v1::LoadSessionResponse = serde_json::from_str(
            r#"{
                "configOptions": [{
                    "id": "resume-model",
                    "name": "Model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "sonnet",
                    "options": [
                        {"value": "sonnet", "name": "Sonnet"},
                        {"value": "opus", "name": "Opus"}
                    ]
                }]
            }"#,
        )
        .expect("valid load_session response");

        let state = models_from_load_session(&response).expect("model selector should be found");
        assert_eq!(
            state.switch_channel,
            ModelSwitchChannel::Config {
                config_id: "resume-model".to_string()
            }
        );
        assert_eq!(state.current_model_id.as_deref(), Some("sonnet"));
        assert_eq!(
            state
                .available_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["sonnet", "opus"]
        );

        assert!(
            models_from_load_session(&acp::schema::v1::LoadSessionResponse::new()).is_none(),
            "a bare load response must preserve the caller's existing switch channel"
        );
    }

    #[test]
    fn private_cloud_catalog_does_not_fabricate_a_bare_session_selector() {
        let response: acp::schema::v1::NewSessionResponse =
            serde_json::from_str(r#"{"sessionId": "bare"}"#).expect("valid new_session");
        let state = models_from_new_session(&response);
        assert!(response.config_options.is_none());
        assert!(state.available_models.is_empty());
        assert_eq!(state.current_model_id, None);

        let mut meta = None;
        inject_wta_cloud_catalog(
            &mut meta,
            &[AcpModelInfo {
                id: "cloud-native".into(),
                name: "Cloud Native".into(),
                description: Some("clean probe".into()),
            }],
            "clean_probe",
        )
        .expect("cloud catalog metadata should serialize");
        let catalog = extract_wta_cloud_catalog(&mut meta);

        assert_eq!(catalog.models.len(), 1);
        assert_eq!(catalog.models[0].id, "cloud-native");
        assert_eq!(catalog.source.as_deref(), Some("clean_probe"));
        assert!(meta.is_none(), "private WTA metadata should be consumed");
    }

    #[test]
    fn asynchronous_cloud_catalog_notification_round_trips() {
        let notification = build_wta_cloud_catalog_notification(
            &[AcpModelInfo {
                id: "cloud-later".into(),
                name: "Cloud Later".into(),
                description: None,
            }],
            "clean_probe",
        );
        let parsed = parse_wta_cloud_catalog_notification(&notification)
            .expect("private method should be recognized")
            .expect("private payload should parse");

        assert_eq!(parsed.models.len(), 1);
        assert_eq!(parsed.models[0].id, "cloud-later");
        assert_eq!(parsed.source.as_deref(), Some("clean_probe"));
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
                 _cx| async move {
                    responder.respond_with_error(acp::Error::method_not_found())
                },
                acp::on_receive_request!(),
            )
            .on_receive_notification(
                |_n: acp::schema::v1::AgentNotification, _cx| async move { Ok(()) },
                acp::on_receive_notification!(),
            );
        let (client, client_io_fut) = conn::spawn_client(
            client_builder,
            conn::byte_streams(cw.compat_write(), cr.compat()),
        );

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
        let (_agent, agent_io_fut) = conn::spawn_agent(
            agent_builder,
            conn::byte_streams(aw.compat_write(), ar.compat()),
        );

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
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let hit = Arc::new(AtomicBool::new(false));
            let client = spawn_switch_mock(true, hit.clone());

            let mut channel = ModelSwitchChannel::Config {
                config_id: "model".to_string(),
            };
            let r = apply_session_model(
                &client,
                &mut channel,
                "s-fallback".into(),
                "haiku".to_string(),
            )
            .await;

            assert!(r.is_ok(), "fall back to set_model must succeed, got {r:?}");
            assert!(
                hit.load(Ordering::SeqCst),
                "set_model must be invoked as the fallback"
            );
            assert_eq!(
                channel,
                ModelSwitchChannel::Legacy,
                "MethodNotFound on set_config_option must flip the channel to Legacy"
            );
        });
    }

    #[test]
    fn config_channel_does_not_fall_back_on_other_errors() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&rt, async {
            let hit = Arc::new(AtomicBool::new(false));
            let client = spawn_switch_mock(false, hit.clone());

            let mut channel = ModelSwitchChannel::Config {
                config_id: "model".to_string(),
            };
            let r =
                apply_session_model(&client, &mut channel, "s-other".into(), "haiku".to_string())
                    .await;

            assert!(r.is_err(), "a non-MethodNotFound error must propagate");
            assert!(
                !hit.load(Ordering::SeqCst),
                "set_model must NOT be called for a non-MethodNotFound error"
            );
            assert!(
                matches!(channel, ModelSwitchChannel::Config { .. }),
                "a non-MethodNotFound error must leave the channel on Config"
            );
        });
    }
}
