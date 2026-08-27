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
use providers::{DiscoveryInput, NativeYoloAction, ProviderSessionState};

#[derive(Clone, Debug)]
pub(crate) struct NativeYoloOperation {
    session_id: acp::schema::v1::SessionId,
    enabled: bool,
    sequence: u64,
    generation: Option<u64>,
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
        let Some(previous) = self.sessions.read().unwrap().get(session_id).cloned() else {
            return;
        };
        let Some(channel) = adapter.refresh_config(config_options, &previous) else {
            return;
        };
        self.sessions
            .write()
            .unwrap()
            .insert(session_id.clone(), ProviderSessionState::Available(channel));
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
