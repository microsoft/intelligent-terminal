//! Provider-native ACP Yolo capability discovery and coordination.
//!
//! This module maps master-attested built-in provider identities to exact
//! advertised session contracts, captures restore values per session, and
//! sequences RPCs so stale operations cannot win. The App owns desired Yolo
//! state; ordinary permission requests always remain user-selected.

mod providers;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use agent_client_protocol as acp;
use providers::{ConfigSpec, ModeSpec, ProviderSpec};

#[derive(Clone, Debug, PartialEq, Eq)]
enum NativeYoloAction {
    SetConfigOption { config_id: String, value: String },
    SetMode { mode_id: String },
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum NativeYoloChannel {
    ConfigOption {
        config_id: String,
        enable_value: String,
        restore_value: String,
        current_value: String,
    },
    Mode {
        enable_mode_id: String,
        restore_mode_id: String,
        current_mode_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SessionNativeYolo {
    Available(NativeYoloChannel),
    UnsupportedProvider { provider: String },
    MissingCapability { provider: String, loaded: bool },
}

#[derive(Clone, Debug)]
pub(crate) struct NativeYoloOperation {
    session_id: acp::schema::v1::SessionId,
    enabled: bool,
    sequence: u64,
    generation: Option<u64>,
}

pub(crate) struct NativeYoloState {
    agent_id: RwLock<Option<String>>,
    sessions: RwLock<HashMap<acp::schema::v1::SessionId, SessionNativeYolo>>,
    session_generations: Mutex<HashMap<acp::schema::v1::SessionId, u64>>,
    operation_gates: Mutex<HashMap<acp::schema::v1::SessionId, Arc<tokio::sync::Mutex<()>>>>,
    desired_operations: Mutex<HashMap<acp::schema::v1::SessionId, u64>>,
    next_generation: AtomicU64,
    next_operation: AtomicU64,
}

impl NativeYoloState {
    pub(crate) fn new() -> Self {
        Self {
            agent_id: RwLock::new(None),
            sessions: RwLock::new(HashMap::new()),
            session_generations: Mutex::new(HashMap::new()),
            operation_gates: Mutex::new(HashMap::new()),
            desired_operations: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
            next_operation: AtomicU64::new(1),
        }
    }

    pub(crate) fn set_resolved_agent_id(&self, agent_id: Option<&str>) {
        let next = agent_id.map(str::to_string);
        let mut current = self.agent_id.write().unwrap();
        if *current != next {
            *current = next;
            self.sessions.write().unwrap().clear();
            self.session_generations.lock().unwrap().clear();
            self.operation_gates.lock().unwrap().clear();
            self.desired_operations.lock().unwrap().clear();
        }
    }

    pub(crate) fn record_from_new_session(&self, response: &acp::schema::v1::NewSessionResponse) {
        self.record_capability(
            &response.session_id,
            response.config_options.as_deref(),
            response.modes.as_ref(),
            false,
        );
    }

    pub(crate) fn record_from_load_session(
        &self,
        session_id: &acp::schema::v1::SessionId,
        response: &acp::schema::v1::LoadSessionResponse,
    ) {
        self.record_capability(
            session_id,
            response.config_options.as_deref(),
            response.modes.as_ref(),
            true,
        );
    }

    pub(crate) fn record_from_config_update(
        &self,
        session_id: &acp::schema::v1::SessionId,
        config_options: &[acp::schema::v1::SessionConfigOption],
    ) {
        let Some(spec) = self.provider_spec() else {
            return;
        };
        let Some(config_spec) = spec.config else {
            return;
        };
        let Some(previous) = self.sessions.read().unwrap().get(session_id).cloned() else {
            return;
        };
        let Some(channel) = config_channel(config_options, config_spec, Some(&previous)) else {
            return;
        };
        self.sessions
            .write()
            .unwrap()
            .insert(session_id.clone(), SessionNativeYolo::Available(channel));
    }

    pub(crate) fn record_current_mode(
        &self,
        session_id: &acp::schema::v1::SessionId,
        current_mode_id: &str,
    ) {
        let mut sessions = self.sessions.write().unwrap();
        let Some(SessionNativeYolo::Available(NativeYoloChannel::Mode {
            enable_mode_id,
            restore_mode_id,
            current_mode_id: known_current,
        })) = sessions.get_mut(session_id)
        else {
            return;
        };
        if current_mode_id != enable_mode_id {
            *restore_mode_id = current_mode_id.to_string();
        }
        *known_current = current_mode_id.to_string();
    }

    pub(crate) fn forget_session(&self, session_id: &acp::schema::v1::SessionId) {
        self.sessions.write().unwrap().remove(session_id);
        let tombstone = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.session_generations
            .lock()
            .unwrap()
            .insert(session_id.clone(), tombstone);
        self.desired_operations.lock().unwrap().remove(session_id);
        let mut gates = self.operation_gates.lock().unwrap();
        if gates
            .get(session_id)
            .is_some_and(|gate| Arc::strong_count(gate) == 1)
        {
            gates.remove(session_id);
        }
    }

    pub(crate) fn reserve_operation(
        &self,
        session_id: acp::schema::v1::SessionId,
        enabled: bool,
    ) -> NativeYoloOperation {
        let sequence = self.next_operation.fetch_add(1, Ordering::Relaxed);
        let generation = self
            .session_generations
            .lock()
            .unwrap()
            .get(&session_id)
            .copied();
        self.desired_operations
            .lock()
            .unwrap()
            .insert(session_id.clone(), sequence);
        NativeYoloOperation {
            session_id,
            enabled,
            sequence,
            generation,
        }
    }

    pub(crate) async fn apply(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        session_id: acp::schema::v1::SessionId,
        enabled: bool,
    ) -> Result<(), String> {
        let operation = self.reserve_operation(session_id, enabled);
        self.apply_reserved(conn, operation).await
    }

    pub(crate) async fn apply_reserved(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        operation: NativeYoloOperation,
    ) -> Result<(), String> {
        let gate = self
            .operation_gates
            .lock()
            .unwrap()
            .entry(operation.session_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = gate.lock().await;
        if !self.operation_is_current(&operation) {
            return Ok(());
        }
        let action = self.action_for(&operation.session_id, operation.enabled)?;
        match &action {
            NativeYoloAction::SetConfigOption { config_id, value } => {
                let response = conn
                    .set_session_config_option(acp::schema::v1::SetSessionConfigOptionRequest::new(
                        operation.session_id.clone(),
                        config_id.clone(),
                        value.as_str(),
                    ))
                    .await;
                if !self.operation_is_current(&operation) {
                    return Ok(());
                }
                let response = response.map_err(|error| error.to_string())?;
                self.record_from_config_update(&operation.session_id, &response.config_options);
            }
            NativeYoloAction::SetMode { mode_id } => {
                let response = conn
                    .set_session_mode(acp::schema::v1::SetSessionModeRequest::new(
                        operation.session_id.clone(),
                        mode_id.clone(),
                    ))
                    .await;
                if !self.operation_is_current(&operation) {
                    return Ok(());
                }
                response.map_err(|error| error.to_string())?;
                self.record_current_mode(&operation.session_id, mode_id);
            }
            NativeYoloAction::Noop => {}
        }
        Ok(())
    }

    fn operation_is_current(&self, operation: &NativeYoloOperation) -> bool {
        self.desired_operations
            .lock()
            .unwrap()
            .get(&operation.session_id)
            .copied()
            == Some(operation.sequence)
            && self
                .session_generations
                .lock()
                .unwrap()
                .get(&operation.session_id)
                .copied()
                == operation.generation
    }

    fn action_for(
        &self,
        session_id: &acp::schema::v1::SessionId,
        enabled: bool,
    ) -> Result<NativeYoloAction, String> {
        match self.sessions.read().unwrap().get(session_id) {
            Some(SessionNativeYolo::Available(NativeYoloChannel::ConfigOption {
                config_id,
                enable_value,
                restore_value,
                ..
            })) => Ok(NativeYoloAction::SetConfigOption {
                config_id: config_id.clone(),
                value: if enabled { enable_value } else { restore_value }.clone(),
            }),
            Some(SessionNativeYolo::Available(NativeYoloChannel::Mode {
                enable_mode_id,
                restore_mode_id,
                ..
            })) => Ok(NativeYoloAction::SetMode {
                mode_id: if enabled {
                    enable_mode_id
                } else {
                    restore_mode_id
                }
                .clone(),
            }),
            Some(SessionNativeYolo::UnsupportedProvider { provider }) if !enabled => {
                Ok(NativeYoloAction::Noop)
            }
            Some(SessionNativeYolo::UnsupportedProvider { provider }) => {
                Err(format!("{provider} does not support ACP session Yolo mode"))
            }
            Some(SessionNativeYolo::MissingCapability { loaded: false, .. }) if !enabled => {
                Ok(NativeYoloAction::Noop)
            }
            Some(SessionNativeYolo::MissingCapability { provider, .. }) => Err(format!(
                "{provider} did not advertise its expected ACP session Yolo capability"
            )),
            None => Err("the session has no advertised ACP Yolo capability".to_string()),
        }
    }

    fn record_capability(
        &self,
        session_id: &acp::schema::v1::SessionId,
        config_options: Option<&[acp::schema::v1::SessionConfigOption]>,
        modes: Option<&acp::schema::v1::SessionModeState>,
        loaded: bool,
    ) {
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        self.session_generations
            .lock()
            .unwrap()
            .insert(session_id.clone(), generation);
        self.desired_operations.lock().unwrap().remove(session_id);
        let provider = self
            .agent_id
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "current provider".to_string());
        let previous = self.sessions.read().unwrap().get(session_id).cloned();
        let capability = match providers::lookup(&provider).and_then(|adapter| adapter.spec()) {
            Some(spec) => {
                let discovered = spec
                    .config
                    .and_then(|config| {
                        config_options
                            .and_then(|options| config_channel(options, config, previous.as_ref()))
                    })
                    .or_else(|| {
                        spec.mode.and_then(|mode| {
                            modes.and_then(|state| mode_channel(state, mode, previous.as_ref()))
                        })
                    });
                discovered
                    .map(SessionNativeYolo::Available)
                    .unwrap_or_else(|| SessionNativeYolo::MissingCapability { provider, loaded })
            }
            None => SessionNativeYolo::UnsupportedProvider { provider },
        };
        self.sessions
            .write()
            .unwrap()
            .insert(session_id.clone(), capability);
    }

    fn provider_spec(&self) -> Option<ProviderSpec> {
        self.agent_id
            .read()
            .unwrap()
            .as_deref()
            .and_then(providers::lookup)
            .and_then(|adapter| adapter.spec())
    }
}

fn config_channel(
    options: &[acp::schema::v1::SessionConfigOption],
    spec: ConfigSpec,
    previous: Option<&SessionNativeYolo>,
) -> Option<NativeYoloChannel> {
    let option = options.iter().find(|option| {
        option.id.0.as_ref() == spec.id
            && category_name(option.category.as_ref()).as_deref() == Some(spec.category)
    })?;
    let acp::schema::v1::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };
    let values = select_values(select)?;
    let current_value = select.current_value.0.as_ref();
    if !values.contains(&spec.enable_value) || !values.contains(&current_value) {
        return None;
    }
    let restore_value = if current_value != spec.enable_value {
        current_value.to_string()
    } else {
        previous_restore_config(previous, spec.id, spec.enable_value)
            .filter(|value| values.contains(&value.as_str()))
            .unwrap_or_else(|| spec.default_restore_value.to_string())
    };
    values
        .contains(&restore_value.as_str())
        .then(|| NativeYoloChannel::ConfigOption {
            config_id: spec.id.to_string(),
            enable_value: spec.enable_value.to_string(),
            restore_value,
            current_value: current_value.to_string(),
        })
}

fn mode_channel(
    state: &acp::schema::v1::SessionModeState,
    spec: ModeSpec,
    previous: Option<&SessionNativeYolo>,
) -> Option<NativeYoloChannel> {
    let available = state
        .available_modes
        .iter()
        .map(|mode| mode.id.0.as_ref())
        .collect::<Vec<_>>();
    let current_mode_id = state.current_mode_id.0.as_ref();
    if !available.contains(&spec.enable_mode_id) || !available.contains(&current_mode_id) {
        return None;
    }
    let restore_mode_id = if current_mode_id != spec.enable_mode_id {
        current_mode_id.to_string()
    } else {
        previous_restore_mode(previous, spec.enable_mode_id)
            .filter(|value| available.contains(&value.as_str()))
            .unwrap_or_else(|| spec.default_restore_mode_id.to_string())
    };
    available
        .contains(&restore_mode_id.as_str())
        .then(|| NativeYoloChannel::Mode {
            enable_mode_id: spec.enable_mode_id.to_string(),
            restore_mode_id,
            current_mode_id: current_mode_id.to_string(),
        })
}

fn previous_restore_config(
    previous: Option<&SessionNativeYolo>,
    config_id: &str,
    enable_value: &str,
) -> Option<String> {
    match previous {
        Some(SessionNativeYolo::Available(NativeYoloChannel::ConfigOption {
            config_id: previous_id,
            restore_value,
            ..
        })) if previous_id == config_id && restore_value != enable_value => {
            Some(restore_value.clone())
        }
        _ => None,
    }
}

fn previous_restore_mode(
    previous: Option<&SessionNativeYolo>,
    enable_mode_id: &str,
) -> Option<String> {
    match previous {
        Some(SessionNativeYolo::Available(NativeYoloChannel::Mode {
            enable_mode_id: previous_id,
            restore_mode_id,
            ..
        })) if previous_id == enable_mode_id && restore_mode_id != enable_mode_id => {
            Some(restore_mode_id.clone())
        }
        _ => None,
    }
}

fn select_values(select: &acp::schema::v1::SessionConfigSelect) -> Option<Vec<&str>> {
    match &select.options {
        acp::schema::v1::SessionConfigSelectOptions::Ungrouped(options) => Some(
            options
                .iter()
                .map(|option| option.value.0.as_ref())
                .collect(),
        ),
        acp::schema::v1::SessionConfigSelectOptions::Grouped(groups) => Some(
            groups
                .iter()
                .flat_map(|group| group.options.iter())
                .map(|option| option.value.0.as_ref())
                .collect(),
        ),
        _ => None,
    }
}

fn category_name(
    category: Option<&acp::schema::v1::SessionConfigOptionCategory>,
) -> Option<String> {
    serde_json::to_value(category?)
        .ok()?
        .as_str()
        .map(str::to_string)
}

#[cfg(test)]
#[path = "native_yolo_tests.rs"]
mod tests;
