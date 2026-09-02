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
use std::time::Duration;

use agent_client_protocol as acp;
use providers::{
    ChannelDiscovery, DiscoveryInput, NativeYoloAction, NativeYoloChannel, ProviderSessionState,
};

pub(super) const NATIVE_YOLO_RPC_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub(super) struct NativeYoloApplyError {
    message: String,
    restart_required: bool,
}

impl NativeYoloApplyError {
    fn known(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            restart_required: false,
        }
    }

    fn timed_out(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            restart_required: true,
        }
    }

    fn unrestorable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            restart_required: true,
        }
    }

    pub(super) fn restart_required(&self) -> bool {
        self.restart_required
    }

    pub(super) fn requiring_restart(mut self) -> Self {
        self.restart_required = true;
        self
    }
}

impl std::fmt::Display for NativeYoloApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NativeYoloOperation {
    session_id: acp::schema::v1::SessionId,
    enabled: bool,
    sequence: u64,
    generation: Option<u64>,
}

impl NativeYoloOperation {
    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }
}

pub(crate) struct NativeYoloState {
    agent_id: RwLock<Option<String>>,
    sessions: RwLock<HashMap<acp::schema::v1::SessionId, ProviderSessionState>>,
    session_generations: Mutex<HashMap<acp::schema::v1::SessionId, u64>>,
    operation_gates: Mutex<HashMap<acp::schema::v1::SessionId, Arc<tokio::sync::Mutex<()>>>>,
    desired_operations: Mutex<HashMap<acp::schema::v1::SessionId, u64>>,
    next_generation: AtomicU64,
    next_operation: AtomicU64,
}

struct OperationGateLease<'a> {
    gates: &'a Mutex<HashMap<acp::schema::v1::SessionId, Arc<tokio::sync::Mutex<()>>>>,
    session_id: acp::schema::v1::SessionId,
    gate: Arc<tokio::sync::Mutex<()>>,
}

impl Drop for OperationGateLease<'_> {
    fn drop(&mut self) {
        let mut gates = self.gates.lock().unwrap();
        if gates.get(&self.session_id).is_some_and(|current| {
            Arc::ptr_eq(current, &self.gate) && Arc::strong_count(current) == 2
        }) {
            gates.remove(&self.session_id);
        }
    }
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
        let provider = self.provider_id();
        let Some(adapter) = providers::lookup(&provider) else {
            return;
        };
        let mut sessions = self.sessions.write().unwrap();
        let Some(previous) = sessions.get(session_id).cloned() else {
            return;
        };
        let next = match adapter.refresh_config(config_options, &previous) {
            ChannelDiscovery::Available(channel) => ProviderSessionState::Available(channel),
            ChannelDiscovery::UnrestorablePrivileged => match previous {
                // A separately advertised mode remains a valid restore path
                // even when the config-only update is already privileged and
                // omits its own restore value.
                mode @ ProviderSessionState::Available(NativeYoloChannel::Mode { .. }) => mode,
                _ => ProviderSessionState::UnrestorablePrivileged,
            },
            ChannelDiscovery::Missing => match previous {
                // A config-only update does not revoke a separately advertised mode.
                mode @ ProviderSessionState::Available(NativeYoloChannel::Mode { .. }) => mode,
                // Once a config capability was available, its disappearance makes the
                // provider's current privileged state uncertain. Disable must fail closed.
                ProviderSessionState::Available(NativeYoloChannel::ConfigOption { .. })
                | ProviderSessionState::UnrestorablePrivileged => {
                    ProviderSessionState::MissingCapability { loaded: true }
                }
                state => state,
            },
        };
        sessions.insert(session_id.clone(), next);
    }

    fn record_acknowledged_config_update(
        &self,
        operation: &NativeYoloOperation,
        config_id: &str,
        value: &str,
        config_options: &[acp::schema::v1::SessionConfigOption],
    ) -> Result<(), NativeYoloApplyError> {
        let acknowledged = config_options.iter().any(|option| {
            option.id.0.as_ref() == config_id
                && matches!(
                    &option.kind,
                    acp::schema::v1::SessionConfigKind::Select(select)
                        if select.current_value.0.as_ref() == value
                )
        });
        if !acknowledged {
            return Err(NativeYoloApplyError::known(format!(
                "provider did not acknowledge config option '{config_id}' value '{value}' for session '{}'",
                operation.session_id
            ))
            .requiring_restart());
        }
        self.record_from_config_update(&operation.session_id, config_options);
        if operation.enabled {
            let reversible = matches!(
                self.sessions.read().unwrap().get(&operation.session_id),
                Some(ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
                    config_id: acknowledged_id,
                    enable_value,
                    restore_value,
                    current_value,
                })) if acknowledged_id == config_id
                    && enable_value == value
                    && current_value == value
                    && restore_value != value
            );
            if !reversible {
                return Err(NativeYoloApplyError::known(format!(
                    "provider did not acknowledge a reversible config option '{config_id}' value '{value}' for session '{}'",
                    operation.session_id
                ))
                .requiring_restart());
            }
        }
        Ok(())
    }

    pub(crate) fn native_config_selection(
        &self,
        session_id: &acp::schema::v1::SessionId,
        config_id: &str,
        value: &str,
    ) -> Option<bool> {
        let sessions = self.sessions.read().unwrap();
        let Some(ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
            config_id: native_config_id,
            enable_value,
            ..
        })) = sessions.get(session_id)
        else {
            return None;
        };
        if native_config_id != config_id {
            return None;
        }
        Some(enable_value == value)
    }

    pub(crate) fn is_native_config_option(
        &self,
        session_id: &acp::schema::v1::SessionId,
        config_id: &str,
    ) -> bool {
        matches!(
            self.sessions.read().unwrap().get(session_id),
            Some(ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
                config_id: native_config_id,
                ..
            })) if native_config_id == config_id
        )
    }

    pub(crate) fn privileged_agent_command<'a>(&self, input: &'a str) -> Option<&'a str> {
        let command_name = input
            .trim_start()
            .strip_prefix('/')?
            .split_whitespace()
            .next()?;
        let provider = self.provider_id();
        providers::lookup(&provider)?
            .is_privileged_agent_command(command_name)
            .then_some(command_name)
    }

    pub(super) async fn apply_native_config_reserved_with_policy_timeout(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        operation: NativeYoloOperation,
        config_id: &str,
        value: &str,
        yolo_state: &crate::app_contracts::SharedYoloState,
        rpc_timeout: Duration,
    ) -> Result<Option<Vec<acp::schema::v1::SessionConfigOption>>, NativeYoloApplyError> {
        let lease = {
            let gate = self
                .operation_gates
                .lock()
                .unwrap()
                .entry(operation.session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            OperationGateLease {
                gates: &self.operation_gates,
                session_id: operation.session_id.clone(),
                gate,
            }
        };
        let _guard = lease.gate.lock().await;
        if !self.operation_is_current(&operation) {
            return Ok(None);
        }
        if operation.enabled && yolo_state.lock().unwrap().policy_blocked() {
            return Err(NativeYoloApplyError::known(
                "the AllowYoloMode policy blocks this privileged provider mode".to_string(),
            ));
        }
        let valid_transition = matches!(
            self.sessions.read().unwrap().get(&operation.session_id),
            Some(ProviderSessionState::Available(NativeYoloChannel::ConfigOption {
                config_id: native_config_id,
                enable_value,
                ..
            })) if native_config_id == config_id
                && (operation.enabled == (enable_value == value))
        );
        if !valid_transition {
            return Err(NativeYoloApplyError::known(
                "the requested config value is not the provider's native Yolo transition"
                    .to_string(),
            ));
        }

        let response = tokio::time::timeout(
            rpc_timeout,
            conn.set_session_config_option(
                acp::schema::v1::SetSessionConfigOptionRequest::new(
                    operation.session_id.clone(),
                    config_id.to_string(),
                    value,
                ),
            ),
        )
        .await
        .map_err(|_| {
            NativeYoloApplyError::timed_out(format!(
                "provider-native Yolo RPC timed out while setting config option '{config_id}' for session '{}'",
                operation.session_id
            ))
        })?;
        if !self.operation_is_current(&operation) {
            return Ok(None);
        }
        let response = response.map_err(|error| NativeYoloApplyError::known(error.to_string()))?;
        self.record_acknowledged_config_update(
            &operation,
            config_id,
            value,
            &response.config_options,
        )?;
        Ok(Some(response.config_options))
    }

    pub(crate) fn record_current_mode(
        &self,
        session_id: &acp::schema::v1::SessionId,
        current_mode_id: &str,
    ) {
        let provider = self.provider_id();
        let Some(adapter) = providers::lookup(&provider) else {
            return;
        };
        let mut sessions = self.sessions.write().unwrap();
        let Some(state) = sessions.get_mut(session_id) else {
            return;
        };
        adapter.observe_current_mode(state, current_mode_id);
    }

    pub(crate) fn forget_session(&self, session_id: &acp::schema::v1::SessionId) {
        self.sessions.write().unwrap().remove(session_id);
        self.session_generations.lock().unwrap().remove(session_id);
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
        self.apply_reserved(conn, operation)
            .await
            .map_err(|error| error.to_string())
    }

    pub(super) async fn apply_reserved(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        operation: NativeYoloOperation,
    ) -> Result<(), NativeYoloApplyError> {
        self.apply_reserved_with_timeout(conn, operation, NATIVE_YOLO_RPC_TIMEOUT)
            .await
    }

    pub(super) async fn apply_reserved_with_timeout(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        operation: NativeYoloOperation,
        rpc_timeout: Duration,
    ) -> Result<(), NativeYoloApplyError> {
        self.apply_reserved_with_policy_timeout(conn, operation, rpc_timeout, None)
            .await
    }

    pub(super) async fn apply_reserved_with_policy_timeout(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        operation: NativeYoloOperation,
        rpc_timeout: Duration,
        yolo_state: Option<&crate::app_contracts::SharedYoloState>,
    ) -> Result<(), NativeYoloApplyError> {
        self.apply_reserved_with_policy_timeout_and_config(conn, operation, rpc_timeout, yolo_state)
            .await
            .map(|_| ())
    }

    pub(super) async fn apply_reserved_with_policy_timeout_and_config(
        &self,
        conn: &crate::protocol::acp::conn::ClientLink,
        operation: NativeYoloOperation,
        rpc_timeout: Duration,
        yolo_state: Option<&crate::app_contracts::SharedYoloState>,
    ) -> Result<Option<Vec<acp::schema::v1::SessionConfigOption>>, NativeYoloApplyError> {
        let lease = {
            let gate = self
                .operation_gates
                .lock()
                .unwrap()
                .entry(operation.session_id.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone();
            OperationGateLease {
                gates: &self.operation_gates,
                session_id: operation.session_id.clone(),
                gate,
            }
        };
        let _guard = lease.gate.lock().await;
        let result = async {
            if !self.operation_is_current(&operation) {
                return Ok(None);
            }
            if operation.enabled
                && yolo_state.is_some_and(|state| state.lock().unwrap().policy_blocked())
            {
                return Err(NativeYoloApplyError::known(
                    "the AllowYoloMode policy blocks provider-native Yolo".to_string(),
                ));
            }
            if matches!(
                self.sessions.read().unwrap().get(&operation.session_id),
                Some(ProviderSessionState::UnrestorablePrivileged)
            ) {
                return Err(NativeYoloApplyError::unrestorable(format!(
                    "{} is already in its ACP session Yolo mode without an advertised restore value",
                    self.provider_id()
                )));
            }
            let action = self
                .action_for(&operation.session_id, operation.enabled)
                .map_err(NativeYoloApplyError::known)?;
            let config_options = match &action {
                NativeYoloAction::SetConfigOption { config_id, value } => {
                    let response = tokio::time::timeout(
                        rpc_timeout,
                        conn.set_session_config_option(
                            acp::schema::v1::SetSessionConfigOptionRequest::new(
                                operation.session_id.clone(),
                                config_id.clone(),
                                value.as_str(),
                            ),
                        ),
                    )
                    .await;
                    let response = response.map_err(|_| {
                        NativeYoloApplyError::timed_out(format!(
                            "provider-native Yolo RPC timed out while setting config option '{config_id}' for session '{}'",
                            operation.session_id
                        ))
                    })?;
                    if !self.operation_is_current(&operation) {
                        return Ok(None);
                    }
                    let response = response
                        .map_err(|error| NativeYoloApplyError::known(error.to_string()))?;
                    self.record_acknowledged_config_update(
                        &operation,
                        config_id,
                        value,
                        &response.config_options,
                    )?;
                    Some(response.config_options)
                }
                NativeYoloAction::SetMode { mode_id } => {
                    let response = tokio::time::timeout(
                        rpc_timeout,
                        conn.set_session_mode(acp::schema::v1::SetSessionModeRequest::new(
                            operation.session_id.clone(),
                            mode_id.clone(),
                        )),
                    )
                    .await;
                    let response = response.map_err(|_| {
                        NativeYoloApplyError::timed_out(format!(
                            "provider-native Yolo RPC timed out while setting mode '{mode_id}' for session '{}'",
                            operation.session_id
                        ))
                    })?;
                    if !self.operation_is_current(&operation) {
                        return Ok(None);
                    }
                    response
                        .map_err(|error| NativeYoloApplyError::known(error.to_string()))?;
                    self.record_current_mode(&operation.session_id, mode_id);
                    None
                }
                NativeYoloAction::Noop => None,
            };
            Ok(config_options)
        }
        .await;
        result
    }

    pub(super) fn operation_is_current(&self, operation: &NativeYoloOperation) -> bool {
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
        let provider = self.provider_id();
        let sessions = self.sessions.read().unwrap();
        let Some(state) = sessions.get(session_id) else {
            return Err("the session has no advertised ACP Yolo capability".to_string());
        };
        match providers::lookup(&provider) {
            Some(adapter) if enabled => adapter.enable(state),
            Some(adapter) => adapter.disable(state),
            None => providers::unsupported_action(&provider, enabled),
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
        let provider = self.provider_id();
        let previous = self.sessions.read().unwrap().get(session_id).cloned();
        let capability = match providers::lookup(&provider) {
            Some(adapter) => adapter.discover(DiscoveryInput {
                config_options,
                modes,
                previous: previous.as_ref(),
                loaded,
            }),
            None => ProviderSessionState::Unsupported,
        };
        self.sessions
            .write()
            .unwrap()
            .insert(session_id.clone(), capability);
    }

    fn provider_id(&self) -> String {
        self.agent_id
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "current provider".to_string())
    }
}

#[cfg(test)]
#[path = "native_yolo_tests.rs"]
mod tests;
