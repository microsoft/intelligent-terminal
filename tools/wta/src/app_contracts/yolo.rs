use std::collections::HashSet;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct YoloState {
    global_default: bool,
    client_reconciled_sessions: HashSet<String>,
    policy_blocked: bool,
}

pub type SharedYoloState = Arc<Mutex<YoloState>>;

impl YoloState {
    pub fn new(global_default: bool, policy_blocked: bool) -> Self {
        Self {
            global_default: global_default && !policy_blocked,
            client_reconciled_sessions: HashSet::new(),
            policy_blocked,
        }
    }

    pub fn effective(&self, _session_id: &str) -> bool {
        !self.policy_blocked && self.global_default
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.client_reconciled_sessions.remove(session_id);
    }

    pub fn clear_sessions(&mut self) {
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
    fn policy_block_fails_closed() {
        let mut state = YoloState::new(true, false);
        state.update_runtime(true, true);
        assert!(!state.effective("session"));
        assert!(!state.effective("other"));

        state.update_runtime(false, false);
        assert!(!state.effective("session"));
    }
}
