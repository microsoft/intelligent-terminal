use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct YoloState {
    global_default: bool,
    session_overrides: HashMap<String, bool>,
    client_reconciled_sessions: HashSet<String>,
    policy_blocked: bool,
}

pub type SharedYoloState = Arc<Mutex<YoloState>>;

impl YoloState {
    pub fn new(global_default: bool, policy_blocked: bool) -> Self {
        Self {
            global_default: global_default && !policy_blocked,
            session_overrides: HashMap::new(),
            client_reconciled_sessions: HashSet::new(),
            policy_blocked,
        }
    }

    pub fn effective(&self, session_id: &str) -> bool {
        !self.policy_blocked
            && self
                .session_overrides
                .get(session_id)
                .copied()
                .unwrap_or(self.global_default)
    }

    pub fn set_session_override(&mut self, session_id: String, enabled: bool) {
        self.session_overrides.insert(session_id, enabled);
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.session_overrides.remove(session_id);
        self.client_reconciled_sessions.remove(session_id);
    }

    pub fn clear_sessions(&mut self) {
        self.session_overrides.clear();
        self.client_reconciled_sessions.clear();
    }

    pub fn mark_client_reconciled(&mut self, session_id: String) {
        self.client_reconciled_sessions.insert(session_id);
    }

    pub fn take_client_reconciled(&mut self, session_id: &str) -> bool {
        self.client_reconciled_sessions.remove(session_id)
    }

    pub fn update_runtime(&mut self, global_default: bool, policy_blocked: bool) {
        self.policy_blocked = policy_blocked;
        self.global_default = global_default && !policy_blocked;
        if policy_blocked {
            self.session_overrides.clear();
        }
    }

    pub fn global_default(&self) -> bool {
        self.global_default
    }

    pub fn policy_blocked(&self) -> bool {
        self.policy_blocked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_session_override_wins_over_global_default() {
        let mut state = YoloState::new(false, false);
        state.set_session_override("session".to_string(), true);
        assert!(state.effective("session"));

        state.update_runtime(true, false);
        state.set_session_override("session".to_string(), false);
        assert!(!state.effective("session"));
        assert!(state.effective("other"));
    }

    #[test]
    fn policy_block_fails_closed_and_drops_session_overrides() {
        let mut state = YoloState::new(false, false);
        state.set_session_override("session".to_string(), true);

        state.update_runtime(true, true);
        assert!(!state.effective("session"));
        assert!(!state.effective("other"));

        state.update_runtime(false, false);
        assert!(!state.effective("session"));
    }
}
