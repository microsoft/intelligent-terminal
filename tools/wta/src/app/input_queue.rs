use super::*;

pub(super) const INPUT_QUEUE_CAPACITY: usize = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InputGateResult {
    Submitted,
    Queued,
    Full,
}

impl App {
    pub(super) fn input_queue_is_full(&self, tab_id: &str) -> bool {
        self.tab_sessions
            .get(tab_id)
            .is_some_and(|tab| tab.pending_inputs.len() >= INPUT_QUEUE_CAPACITY)
    }

    /// Admit prepared input using only readiness, capacity, and FIFO order.
    pub(super) fn gate_input(&mut self, tab_id: &str, envelope: InputEnvelope) -> InputGateResult {
        let reconfiguration_pending = self.prompt_reconfiguration_pending_for_tab(tab_id);
        let submit_now = {
            let tab = self.tab_mut(tab_id);
            tab.pending_inputs.is_empty()
                && tab.turn.accepts_new_prompt()
                && tab.turn.recommendations().is_none()
                && !reconfiguration_pending
        };
        if submit_now {
            self.dispatch_input(tab_id, envelope, false);
            return InputGateResult::Submitted;
        }

        let tab = self.tab_mut(tab_id);
        if tab.pending_inputs.len() >= INPUT_QUEUE_CAPACITY {
            return InputGateResult::Full;
        }
        tab.pending_inputs.push_back(envelope);
        InputGateResult::Queued
    }

    pub(super) fn drain_input_queue(&mut self, tab_id: &str) -> bool {
        if self.state != ConnectionState::Connected {
            return false;
        }
        if !self.tab_sessions.contains_key(tab_id) {
            return false;
        }
        if self.prompt_reconfiguration_pending_for_tab(tab_id) {
            return false;
        }
        let envelope = {
            let tab = self.tab_mut(tab_id);
            if !tab.turn.accepts_new_prompt() || tab.turn.recommendations().is_some() {
                return false;
            }
            tab.pending_inputs.pop_front()
        };
        let Some(envelope) = envelope else {
            return false;
        };
        self.dispatch_input(tab_id, envelope, true);
        true
    }

    pub(super) fn schedule_input_queue_drain(&self, session_id: &str) {
        let Some(tab_id) = self.bound_tab_for_session(session_id) else {
            return;
        };
        self.schedule_input_queue_drain_for_tab(&tab_id);
    }

    pub(super) fn schedule_input_queue_drain_for_tab(&self, tab_id: &str) {
        let Some(tab) = self.tab_sessions.get(tab_id) else {
            return;
        };
        if tab.pending_inputs.is_empty() {
            return;
        }
        let Some(event_tx) = self.event_tx.as_ref() else {
            tracing::debug!(
                target: "input_queue",
                tab_id,
                "input drain deferred because the app event channel is unavailable",
            );
            return;
        };
        if event_tx
            .send(AppEvent::DrainInputQueue {
                tab_id: tab_id.to_string(),
            })
            .is_err()
        {
            tracing::warn!(
                target: "input_queue",
                tab_id,
                "failed to enqueue input drain event",
            );
        }
    }
}
