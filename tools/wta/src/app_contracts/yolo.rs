use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutomaticYoloDirective {
    Enable,
    Disable,
    NoOpinion,
}

impl AutomaticYoloDirective {
    pub fn target(self) -> Option<bool> {
        match self {
            Self::Enable => Some(true),
            Self::Disable => Some(false),
            Self::NoOpinion => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum YoloControlOwner {
    Automatic,
    Manual,
    ProviderRestored,
}

impl YoloControlOwner {
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::ProviderRestored => "provider-restored",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "automatic" => Some(Self::Automatic),
            "manual" => Some(Self::Manual),
            "provider-restored" => Some(Self::ProviderRestored),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub struct YoloState {
    automatic_target: bool,
    client_reconciled_sessions: HashMap<String, bool>,
    session_owners: HashMap<String, YoloControlOwner>,
    policy_blocked: bool,
}

pub type SharedYoloState = Arc<Mutex<YoloState>>;

impl YoloState {
    pub fn new(automatic_target: bool, policy_blocked: bool) -> Self {
        Self {
            automatic_target: automatic_target && !policy_blocked,
            client_reconciled_sessions: HashMap::new(),
            session_owners: HashMap::new(),
            policy_blocked,
        }
    }

    pub fn automatic_directive(&self, session_id: &str) -> AutomaticYoloDirective {
        if !self.can_user_request_enable() {
            return AutomaticYoloDirective::Disable;
        }

        match self.session_owners.get(session_id) {
            Some(YoloControlOwner::Manual | YoloControlOwner::ProviderRestored) => {
                AutomaticYoloDirective::NoOpinion
            }
            Some(YoloControlOwner::Automatic) | None => {
                if self.automatic_target {
                    AutomaticYoloDirective::Enable
                } else {
                    AutomaticYoloDirective::Disable
                }
            }
        }
    }

    pub fn mark_automatic(&mut self, session_id: impl Into<String>) {
        self.session_owners
            .insert(session_id.into(), YoloControlOwner::Automatic);
    }

    pub fn mark_automatic_if_unowned_or_automatic(&mut self, session_id: impl Into<String>) {
        let session_id = session_id.into();
        if matches!(
            self.session_owners.get(&session_id),
            None | Some(YoloControlOwner::Automatic)
        ) {
            self.mark_automatic(session_id);
        }
    }

    pub fn mark_manual(&mut self, session_id: impl Into<String>) {
        self.session_owners
            .insert(session_id.into(), YoloControlOwner::Manual);
    }

    pub fn mark_manual_if_allowed(&mut self, session_id: impl Into<String>) -> bool {
        if !self.can_user_request_enable() {
            return false;
        }
        let session_id = session_id.into();
        if self.owner(&session_id) == Some(YoloControlOwner::Manual) {
            return false;
        }
        self.mark_manual(session_id);
        true
    }

    pub fn mark_provider_restored(&mut self, session_id: impl Into<String>) {
        self.session_owners
            .insert(session_id.into(), YoloControlOwner::ProviderRestored);
    }

    pub fn mark_owner(&mut self, session_id: impl Into<String>, owner: YoloControlOwner) {
        self.session_owners.insert(session_id.into(), owner);
    }

    pub fn owner(&self, session_id: &str) -> Option<YoloControlOwner> {
        self.session_owners.get(session_id).copied()
    }

    pub fn remove_session(&mut self, session_id: &str) {
        self.client_reconciled_sessions.remove(session_id);
        self.session_owners.remove(session_id);
    }

    pub fn clear_sessions(&mut self) {
        self.client_reconciled_sessions.clear();
        self.session_owners.clear();
    }

    pub fn mark_client_reconciled(&mut self, session_id: String, enabled: bool) {
        self.mark_automatic(session_id.clone());
        self.client_reconciled_sessions.insert(session_id, enabled);
    }

    pub fn take_client_reconciled(&mut self, session_id: &str) -> Option<bool> {
        self.client_reconciled_sessions.remove(session_id)
    }

    pub fn update_runtime(&mut self, automatic_target: bool, policy_blocked: bool) {
        self.policy_blocked = policy_blocked;
        self.automatic_target = automatic_target && self.can_user_request_enable();
    }

    pub fn automatic_target(&self) -> bool {
        self.automatic_target
    }

    pub fn can_user_request_enable(&self) -> bool {
        !self.policy_blocked
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
        assert!(state.can_user_request_enable());

        state.update_runtime(true, true);
        assert!(!state.can_user_request_enable());
        assert_eq!(
            state.automatic_directive("session"),
            AutomaticYoloDirective::Disable
        );
        assert_eq!(
            state.automatic_directive("other"),
            AutomaticYoloDirective::Disable
        );

        state.update_runtime(false, false);
        assert!(state.can_user_request_enable());
        assert_eq!(
            state.automatic_directive("session"),
            AutomaticYoloDirective::Disable
        );
    }

    #[test]
    fn automatic_directive_respects_session_owner() {
        let mut state = YoloState::new(true, false);
        assert_eq!(
            state.automatic_directive("new-session"),
            AutomaticYoloDirective::Enable
        );

        state.mark_manual("manual-session");
        state.mark_provider_restored("restored-session");
        state.mark_automatic("automatic-session");
        assert_eq!(
            YoloControlOwner::from_wire(YoloControlOwner::Manual.as_wire()),
            Some(YoloControlOwner::Manual)
        );
        assert_eq!(
            state.automatic_directive("manual-session"),
            AutomaticYoloDirective::NoOpinion
        );
        assert_eq!(
            state.automatic_directive("restored-session"),
            AutomaticYoloDirective::NoOpinion
        );
        assert!(state.mark_manual_if_allowed("changed-owner"));
        assert!(
            !state.mark_manual_if_allowed("changed-owner"),
            "an already-manual session must not report another owner change"
        );
        assert_eq!(
            state.automatic_directive("automatic-session"),
            AutomaticYoloDirective::Enable
        );

        state.update_runtime(false, false);
        assert_eq!(
            state.automatic_directive("manual-session"),
            AutomaticYoloDirective::NoOpinion
        );
        assert_eq!(
            state.automatic_directive("automatic-session"),
            AutomaticYoloDirective::Disable
        );
        assert_eq!(
            state.automatic_directive("new-session"),
            AutomaticYoloDirective::Disable
        );

        state.update_runtime(true, true);
        assert!(!state.mark_manual_if_allowed("blocked-session"));
        assert_eq!(
            state.automatic_directive("manual-session"),
            AutomaticYoloDirective::Disable
        );
        assert_eq!(
            state.automatic_directive("restored-session"),
            AutomaticYoloDirective::Disable
        );
    }

    #[test]
    fn session_lifecycle_clears_owner_and_lazy_reconcile_claims_automatic() {
        let mut state = YoloState::new(true, false);
        state.mark_manual("reused-session");
        assert_eq!(
            state.automatic_directive("reused-session"),
            AutomaticYoloDirective::NoOpinion
        );

        state.remove_session("reused-session");
        assert_eq!(
            state.automatic_directive("reused-session"),
            AutomaticYoloDirective::Enable
        );

        state.mark_provider_restored("cleared-session");
        state.clear_sessions();
        assert_eq!(
            state.automatic_directive("cleared-session"),
            AutomaticYoloDirective::Enable
        );

        state.mark_client_reconciled("lazy-session".to_string(), true);
        state.update_runtime(false, false);
        assert_eq!(
            state.automatic_directive("lazy-session"),
            AutomaticYoloDirective::Disable
        );
    }
}
