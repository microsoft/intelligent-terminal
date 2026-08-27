use super::*;
use std::sync::{Arc, Mutex};

use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

#[derive(Clone)]
struct ConfigApplyBarrier {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

fn discover(agent_id: &str, response: &str, enabled: bool) -> Result<NativeYoloAction, String> {
    let state = NativeYoloState::new();
    state.set_resolved_agent_id(Some(agent_id));
    let response: acp::schema::v1::NewSessionResponse =
        serde_json::from_str(response).expect("valid session response");
    let session_id = response.session_id.clone();
    state.record_from_new_session(&response);
    state.action_for(&session_id, enabled)
}

fn spawn_apply_mock(
    actions: Arc<Mutex<Vec<NativeYoloAction>>>,
    config_barrier: Option<ConfigApplyBarrier>,
) -> crate::protocol::acp::conn::ClientLink {
    use crate::protocol::acp::conn;

    let (client_io, agent_io) = tokio::io::duplex(64 * 1024);
    let (client_read, client_write) = tokio::io::split(client_io);
    let (agent_read, agent_write) = tokio::io::split(agent_io);
    let client_builder = acp::Client
        .builder()
        .name("native-yolo-test-client")
        .on_receive_request(
            |_request: acp::schema::v1::AgentRequest,
             responder: acp::Responder<serde_json::Value>,
             _context| async move {
                responder.respond_with_error(acp::Error::method_not_found())
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            |_notification: acp::schema::v1::AgentNotification, _context| async move { Ok(()) },
            acp::on_receive_notification!(),
        );
    let (client, client_io_future) = conn::spawn_client(
        client_builder,
        conn::byte_streams(client_write.compat_write(), client_read.compat()),
    );

    let config_actions = Arc::clone(&actions);
    let mode_actions = Arc::clone(&actions);
    let agent_builder = acp::Agent
        .builder()
        .name("native-yolo-test-agent")
        .on_receive_request(
            move |request: acp::schema::v1::SetSessionConfigOptionRequest,
                  responder: acp::Responder<acp::schema::v1::SetSessionConfigOptionResponse>,
                  _context| {
                let actions = Arc::clone(&config_actions);
                let barrier = config_barrier.clone();
                async move {
                    let value = request
                        .value
                        .as_value_id()
                        .map(|value| value.0.to_string())
                        .expect("test config value is a string");
                    if value == "on" {
                        if let Some(barrier) = barrier {
                            barrier.started.notify_one();
                            barrier.release.notified().await;
                        }
                    }
                    actions
                        .lock()
                        .unwrap()
                        .push(NativeYoloAction::SetConfigOption {
                            config_id: request.config_id.0.to_string(),
                            value,
                        });
                    responder.respond(acp::schema::v1::SetSessionConfigOptionResponse::new(
                        Vec::new(),
                    ))
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_request(
            move |request: acp::schema::v1::SetSessionModeRequest,
                  responder: acp::Responder<acp::schema::v1::SetSessionModeResponse>,
                  _context| {
                let actions = Arc::clone(&mode_actions);
                async move {
                    actions.lock().unwrap().push(NativeYoloAction::SetMode {
                        mode_id: request.mode_id.0.to_string(),
                    });
                    responder.respond(acp::schema::v1::SetSessionModeResponse::new())
                }
            },
            acp::on_receive_request!(),
        )
        .on_receive_notification(
            |_notification: acp::schema::v1::ClientNotification, _context| async move { Ok(()) },
            acp::on_receive_notification!(),
        );
    let (_agent, agent_io_future) = conn::spawn_agent(
        agent_builder,
        conn::byte_streams(agent_write.compat_write(), agent_read.compat()),
    );

    tokio::task::spawn_local(async move {
        let _ = client_io_future.await;
    });
    tokio::task::spawn_local(async move {
        let _ = agent_io_future.await;
    });
    client
}

#[test]
fn discovers_copilot_allow_all_config_and_restore_value() {
    let response = r#"{
        "sessionId": "copilot-session",
        "configOptions": [{
            "id": "allow_all",
            "name": "Allow All",
            "category": "permissions",
            "type": "select",
            "currentValue": "off",
            "options": [
                {"value": "on", "name": "On"},
                {"value": "off", "name": "Off"}
            ]
        }]
    }"#;

    assert_eq!(
        discover(crate::agent_registry::COPILOT_AGENT_ID, response, true),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "allow_all".to_string(),
            value: "on".to_string(),
        })
    );
    assert_eq!(
        discover(crate::agent_registry::COPILOT_AGENT_ID, response, false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "allow_all".to_string(),
            value: "off".to_string(),
        })
    );
}

#[test]
fn discovers_claude_bypass_permissions_config_and_restore_value() {
    let response = r#"{
        "sessionId": "claude-session",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "default",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }"#;

    assert_eq!(
        discover(crate::agent_registry::CLAUDE_AGENT_ID, response, true),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "bypassPermissions".to_string(),
        })
    );
    assert_eq!(
        discover(crate::agent_registry::CLAUDE_AGENT_ID, response, false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "default".to_string(),
        })
    );
}

#[test]
fn discovers_codex_full_access_config_and_restore_value() {
    let response = r#"{
        "sessionId": "codex-session",
        "modes": {
            "currentModeId": "agent",
            "availableModes": [
                {"id": "agent", "name": "Agent"},
                {"id": "agent-full-access", "name": "Agent (Full Access)"}
            ]
        },
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "agent",
            "options": [
                {"value": "read-only", "name": "Read-only"},
                {"value": "agent", "name": "Agent"},
                {"value": "agent-full-access", "name": "Agent (Full Access)"}
            ]
        }]
    }"#;

    assert_eq!(
        discover(crate::agent_registry::CODEX_AGENT_ID, response, true),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "agent-full-access".to_string(),
        })
    );
    assert_eq!(
        discover(crate::agent_registry::CODEX_AGENT_ID, response, false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "agent".to_string(),
        })
    );
}

#[test]
fn claude_and_codex_use_legacy_mode_when_config_option_is_absent() {
    for (agent_id, enable_mode, restore_mode) in [
        (
            crate::agent_registry::CLAUDE_AGENT_ID,
            "bypassPermissions",
            "plan",
        ),
        (
            crate::agent_registry::CODEX_AGENT_ID,
            "agent-full-access",
            "read-only",
        ),
    ] {
        let response = serde_json::json!({
            "sessionId": format!("{agent_id}-mode-session"),
            "modes": {
                "currentModeId": restore_mode,
                "availableModes": [
                    {"id": restore_mode, "name": "Restore"},
                    {"id": enable_mode, "name": "Enable"}
                ]
            }
        });
        let response = serde_json::to_string(&response).unwrap();

        assert_eq!(
            discover(agent_id, &response, true),
            Ok(NativeYoloAction::SetMode {
                mode_id: enable_mode.to_string(),
            })
        );
        assert_eq!(
            discover(agent_id, &response, false),
            Ok(NativeYoloAction::SetMode {
                mode_id: restore_mode.to_string(),
            })
        );
    }
}

#[test]
fn discovers_gemini_yolo_mode_and_restore_value() {
    let response = r#"{
        "sessionId": "gemini-session",
        "modes": {
            "currentModeId": "default",
            "availableModes": [
                {"id": "default", "name": "Default"},
                {"id": "yolo", "name": "YOLO"}
            ]
        }
    }"#;

    assert_eq!(
        discover(crate::agent_registry::GEMINI_AGENT_ID, response, true),
        Ok(NativeYoloAction::SetMode {
            mode_id: "yolo".to_string(),
        })
    );
    assert_eq!(
        discover(crate::agent_registry::GEMINI_AGENT_ID, response, false),
        Ok(NativeYoloAction::SetMode {
            mode_id: "default".to_string(),
        })
    );
}

#[test]
fn unsupported_agents_do_not_infer_native_yolo_from_lookalike_options() {
    let response = r#"{
        "sessionId": "unsupported-session",
        "configOptions": [{
            "id": "allow_all",
            "name": "Allow All",
            "category": "permissions",
            "type": "select",
            "currentValue": "off",
            "options": [
                {"value": "on", "name": "On"},
                {"value": "off", "name": "Off"}
            ]
        }]
    }"#;

    assert!(discover(crate::agent_registry::OPENCODE_AGENT_ID, response, true).is_err());
    assert!(discover("custom:lookalike", response, true).is_err());
    assert_eq!(
        discover(crate::agent_registry::OPENCODE_AGENT_ID, response, false),
        Ok(NativeYoloAction::Noop)
    );
}

#[test]
fn missing_capability_is_safe_to_disable_only_for_new_sessions() {
    let state = NativeYoloState::new();
    state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
    let new_response = acp::schema::v1::NewSessionResponse::new(acp::schema::v1::SessionId::new(
        "new-without-capability",
    ));
    let new_session_id = new_response.session_id.clone();
    state.record_from_new_session(&new_response);

    assert_eq!(
        state.action_for(&new_session_id, false),
        Ok(NativeYoloAction::Noop)
    );
    let expected =
        Err("copilot did not advertise its expected ACP session Yolo capability".to_string());
    assert_eq!(state.action_for(&new_session_id, true), expected);

    let loaded_session_id = acp::schema::v1::SessionId::new("loaded-without-capability");
    let load_response: acp::schema::v1::LoadSessionResponse =
        serde_json::from_str(r#"{"configOptions": []}"#).unwrap();
    state.record_from_load_session(&loaded_session_id, &load_response);

    assert_eq!(state.action_for(&loaded_session_id, false), expected);
    assert_eq!(state.action_for(&loaded_session_id, true), expected);
}

#[test]
fn restore_values_are_isolated_per_session() {
    let state = NativeYoloState::new();
    state.set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
    for (session_id, current_value) in [("default-session", "default"), ("plan-session", "plan")] {
        let response: acp::schema::v1::NewSessionResponse =
            serde_json::from_value(serde_json::json!({
                "sessionId": session_id,
                "configOptions": [{
                    "id": "mode",
                    "name": "Mode",
                    "category": "mode",
                    "type": "select",
                    "currentValue": current_value,
                    "options": [
                        {"value": "default", "name": "Default"},
                        {"value": "plan", "name": "Plan"},
                        {"value": "bypassPermissions", "name": "Bypass Permissions"}
                    ]
                }]
            }))
            .unwrap();
        state.record_from_new_session(&response);
    }

    assert_eq!(
        state.action_for(&acp::schema::v1::SessionId::new("default-session"), false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "default".to_string(),
        })
    );
    assert_eq!(
        state.action_for(&acp::schema::v1::SessionId::new("plan-session"), false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "plan".to_string(),
        })
    );
}

#[test]
fn config_update_refreshes_the_provider_restore_value() {
    let state = NativeYoloState::new();
    state.set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
    let response: acp::schema::v1::NewSessionResponse = serde_json::from_value(serde_json::json!({
        "sessionId": "config-update-session",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "default",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "plan", "name": "Plan"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }))
    .unwrap();
    let session_id = response.session_id.clone();
    state.record_from_new_session(&response);

    let updated: acp::schema::v1::NewSessionResponse = serde_json::from_value(serde_json::json!({
        "sessionId": "unused",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "plan",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "plan", "name": "Plan"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }))
    .unwrap();
    state.record_from_config_update(&session_id, updated.config_options.as_deref().unwrap());

    assert_eq!(
        state.action_for(&session_id, false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "plan".to_string(),
        })
    );
}

#[test]
fn claude_preserves_restore_value_across_mode_and_config_updates() {
    let state = NativeYoloState::new();
    state.set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
    let response: acp::schema::v1::NewSessionResponse = serde_json::from_value(serde_json::json!({
        "sessionId": "cross-channel-session",
        "modes": {
            "currentModeId": "plan",
            "availableModes": [
                {"id": "default", "name": "Default"},
                {"id": "plan", "name": "Plan"},
                {"id": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }
    }))
    .unwrap();
    let session_id = response.session_id.clone();
    state.record_from_new_session(&response);

    let updated: acp::schema::v1::NewSessionResponse = serde_json::from_value(serde_json::json!({
        "sessionId": "unused",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "bypassPermissions",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "plan", "name": "Plan"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }))
    .unwrap();
    state.record_from_config_update(&session_id, updated.config_options.as_deref().unwrap());

    assert_eq!(
        state.action_for(&session_id, false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "plan".to_string(),
        })
    );
}

#[test]
fn claude_mode_update_refreshes_a_config_channel_restore_value() {
    let state = NativeYoloState::new();
    state.set_resolved_agent_id(Some(crate::agent_registry::CLAUDE_AGENT_ID));
    let response: acp::schema::v1::NewSessionResponse = serde_json::from_value(serde_json::json!({
        "sessionId": "mode-update-session",
        "configOptions": [{
            "id": "mode",
            "name": "Mode",
            "category": "mode",
            "type": "select",
            "currentValue": "default",
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "plan", "name": "Plan"},
                {"value": "bypassPermissions", "name": "Bypass Permissions"}
            ]
        }]
    }))
    .unwrap();
    let session_id = response.session_id.clone();
    state.record_from_new_session(&response);
    state.record_current_mode(&session_id, "plan");

    assert_eq!(
        state.action_for(&session_id, false),
        Ok(NativeYoloAction::SetConfigOption {
            config_id: "mode".to_string(),
            value: "plan".to_string(),
        })
    );
}

#[test]
fn applies_and_restores_config_option_over_acp() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let state = NativeYoloState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let response: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "config-session",
                "configOptions": [{
                    "id": "allow_all", "name": "Allow All", "category": "permissions",
                    "type": "select", "currentValue": "off",
                    "options": [{"value": "on", "name": "On"}, {"value": "off", "name": "Off"}]
                }]
            }"#,
        )
        .unwrap();
        let session_id = response.session_id.clone();
        state.record_from_new_session(&response);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let connection = spawn_apply_mock(Arc::clone(&actions), None);

        state
            .apply(&connection, session_id.clone(), true)
            .await
            .unwrap();
        state.apply(&connection, session_id, false).await.unwrap();

        assert_eq!(
            *actions.lock().unwrap(),
            vec![
                NativeYoloAction::SetConfigOption {
                    config_id: "allow_all".to_string(),
                    value: "on".to_string(),
                },
                NativeYoloAction::SetConfigOption {
                    config_id: "allow_all".to_string(),
                    value: "off".to_string(),
                },
            ]
        );
    });
}

#[test]
fn applies_and_restores_mode_over_acp() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let state = NativeYoloState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::GEMINI_AGENT_ID));
        let response: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "mode-session",
                "modes": {
                    "currentModeId": "default",
                    "availableModes": [
                        {"id": "default", "name": "Default"},
                        {"id": "yolo", "name": "YOLO"}
                    ]
                }
            }"#,
        )
        .unwrap();
        let session_id = response.session_id.clone();
        state.record_from_new_session(&response);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let connection = spawn_apply_mock(Arc::clone(&actions), None);

        state
            .apply(&connection, session_id.clone(), true)
            .await
            .unwrap();
        state.apply(&connection, session_id, false).await.unwrap();

        assert_eq!(
            *actions.lock().unwrap(),
            vec![
                NativeYoloAction::SetMode {
                    mode_id: "yolo".to_string(),
                },
                NativeYoloAction::SetMode {
                    mode_id: "default".to_string(),
                },
            ]
        );
    });
}

#[test]
fn serializes_native_yolo_writes_per_session() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let state = Arc::new(NativeYoloState::new());
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let response: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "ordered-session",
                "configOptions": [{
                    "id": "allow_all", "name": "Allow All", "category": "permissions",
                    "type": "select", "currentValue": "off",
                    "options": [{"value": "on", "name": "On"}, {"value": "off", "name": "Off"}]
                }]
            }"#,
        )
        .unwrap();
        let session_id = response.session_id.clone();
        state.record_from_new_session(&response);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let barrier = ConfigApplyBarrier {
            started: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
        };
        let connection = spawn_apply_mock(Arc::clone(&actions), Some(barrier.clone()));

        let enable = tokio::task::spawn_local({
            let state = Arc::clone(&state);
            let connection = connection.clone();
            let session_id = session_id.clone();
            async move { state.apply(&connection, session_id, true).await }
        });
        barrier.started.notified().await;
        let disable = tokio::task::spawn_local({
            let state = Arc::clone(&state);
            let connection = connection.clone();
            let session_id = session_id.clone();
            async move { state.apply(&connection, session_id, false).await }
        });
        tokio::task::yield_now().await;
        barrier.release.notify_one();

        enable.await.unwrap().unwrap();
        disable.await.unwrap().unwrap();
        assert_eq!(
            *actions.lock().unwrap(),
            vec![
                NativeYoloAction::SetConfigOption {
                    config_id: "allow_all".to_string(),
                    value: "on".to_string(),
                },
                NativeYoloAction::SetConfigOption {
                    config_id: "allow_all".to_string(),
                    value: "off".to_string(),
                },
            ]
        );
    });
}

#[test]
fn newer_reserved_operation_supersedes_older_before_rpc() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let state = NativeYoloState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let response: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "superseded-session",
                "configOptions": [{
                    "id": "allow_all", "name": "Allow All", "category": "permissions",
                    "type": "select", "currentValue": "off",
                    "options": [{"value": "on", "name": "On"}, {"value": "off", "name": "Off"}]
                }]
            }"#,
        )
        .unwrap();
        let session_id = response.session_id.clone();
        state.record_from_new_session(&response);
        let older_enable = state.reserve_operation(session_id.clone(), true);
        let newer_disable = state.reserve_operation(session_id, false);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let connection = spawn_apply_mock(Arc::clone(&actions), None);

        state
            .apply_reserved(&connection, older_enable)
            .await
            .unwrap();
        state
            .apply_reserved(&connection, newer_disable)
            .await
            .unwrap();

        assert_eq!(
            *actions.lock().unwrap(),
            vec![NativeYoloAction::SetConfigOption {
                config_id: "allow_all".to_string(),
                value: "off".to_string(),
            }]
        );
    });
}

#[test]
fn teardown_generation_fences_reserved_operation_for_reused_session_id() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    tokio::task::LocalSet::new().block_on(&runtime, async {
        let state = NativeYoloState::new();
        state.set_resolved_agent_id(Some(crate::agent_registry::COPILOT_AGENT_ID));
        let response: acp::schema::v1::NewSessionResponse = serde_json::from_str(
            r#"{
                "sessionId": "reused-session",
                "configOptions": [{
                    "id": "allow_all", "name": "Allow All", "category": "permissions",
                    "type": "select", "currentValue": "off",
                    "options": [{"value": "on", "name": "On"}, {"value": "off", "name": "Off"}]
                }]
            }"#,
        )
        .unwrap();
        let session_id = response.session_id.clone();
        state.record_from_new_session(&response);
        let stale_enable = state.reserve_operation(session_id.clone(), true);
        state.forget_session(&session_id);
        state.record_from_new_session(&response);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let connection = spawn_apply_mock(Arc::clone(&actions), None);

        state
            .apply_reserved(&connection, stale_enable)
            .await
            .unwrap();

        assert!(actions.lock().unwrap().is_empty());
    });
}
