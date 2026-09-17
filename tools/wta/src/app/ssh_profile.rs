//! Profile-derived Sessions defaults. The SSH history source is deliberately
//! independent of the helper's ACP agent execution source.

use super::*;
use crate::ssh_sessions::SshTarget;

#[cfg(test)]
#[path = "ssh_profile_tests.rs"]
mod tests;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum SessionsProfile {
    #[default]
    Agent,
    Ssh(SshTarget),
    Invalid(String),
}

impl SessionsProfile {
    fn from_startup(target: Option<&str>, port: Option<u16>, error: Option<&str>) -> Self {
        if let Some(error) = error {
            return Self::Invalid(error.to_string());
        }
        match target {
            Some(target) => match SshTarget::new(target, port) {
                Ok(target) => Self::Ssh(target),
                Err(error) => Self::Invalid(format!("{error:#}")),
            },
            None if port.is_some() => {
                Self::Invalid("SSH session profile has a port but no destination.".into())
            }
            None => Self::Agent,
        }
    }

    fn from_wire(value: &serde_json::Value) -> Self {
        if value.is_null() {
            return Self::Agent;
        }
        if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
            return Self::Invalid(error.to_string());
        }
        match serde_json::from_value::<SshTarget>(value.clone()) {
            Ok(target) => Self::Ssh(target),
            Err(error) => Self::Invalid(format!("Invalid SSH session profile metadata: {error}")),
        }
    }
}

impl App {
    pub(crate) fn set_initial_sessions_ssh_profile(
        &mut self,
        target: Option<&str>,
        port: Option<u16>,
        error: Option<&str>,
    ) {
        let tab_id = self.active_tab_key().to_string();
        self.set_sessions_profile(&tab_id, SessionsProfile::from_startup(target, port, error));
    }

    pub(super) fn update_sessions_profile_from_event(&mut self, params: &serde_json::Value) {
        let Some(value) = params.get("sessions_ssh") else {
            return;
        };
        let Some(tab_id) = params
            .get("tab_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            tracing::warn!(target: "ssh_sessions", "ignoring profile update without a tab identity");
            return;
        };
        let Some(window_id) = params
            .get("window_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.is_empty())
        else {
            tracing::warn!(target: "ssh_sessions", "ignoring profile update without a window identity");
            return;
        };
        if self
            .owner_tab_id
            .as_deref()
            .is_some_and(|owner| owner != tab_id)
            || self
                .window_id
                .as_deref()
                .is_some_and(|owner| !owner.is_empty() && owner != window_id)
        {
            return;
        }
        self.set_sessions_profile(tab_id, SessionsProfile::from_wire(value));
    }

    fn set_sessions_profile(&mut self, tab_id: &str, profile: SessionsProfile) {
        let tab = self.tab_mut(tab_id);
        if tab.agents_view.ssh_profile == profile {
            return;
        }
        tab.agents_view.ssh_profile = profile;
        self.cancel_ssh_sessions_fetch(tab_id);
        let tab = self.tab_mut(tab_id);
        tab.agents_view.snapshot = tab.agents_view.snapshot.as_ref().map(|_| Vec::new());
        tab.agents_view.ssh_source = None;
        tab.agents_view.ssh_error = None;
        tab.agents_view.focused_sid = None;
        tab.agents_list_state.select(None);
        self.apply_profile_sessions_source(tab_id);
        let tab = self.tab_mut(tab_id);
        if tab.current_view == View::Agents && tab.agents_view.snapshot.is_some() {
            self.schedule_agents_refetch_for_tab(tab_id);
        }
    }

    pub(super) fn apply_profile_sessions_source(&mut self, tab_id: &str) {
        let tab = self.tab_mut(tab_id);
        let profile = tab.agents_view.ssh_profile.clone();
        let source = match &profile {
            SessionsProfile::Ssh(target) => Some(ssh_session_view::SshSessionsSource {
                target: target.clone(),
                agent_id: self.current_agent_id.clone(),
            }),
            SessionsProfile::Agent | SessionsProfile::Invalid(_) => None,
        };
        if self.tab_mut(tab_id).agents_view.ssh_source != source {
            self.cancel_ssh_sessions_fetch(tab_id);
            let tab = self.tab_mut(tab_id);
            tab.agents_view.ssh_source = source;
            tab.agents_view.snapshot = tab.agents_view.snapshot.as_ref().map(|_| Vec::new());
            tab.agents_view.ssh_error = None;
            tab.agents_view.focused_sid = None;
            tab.agents_list_state.select(None);
        }
        if let SessionsProfile::Invalid(error) = profile {
            self.tab_mut(tab_id).agents_view.ssh_error = Some(error);
        }
    }

    pub(super) fn block_invalid_ssh_profile_refetch(&mut self, tab_id: &str) -> bool {
        let tab = self.tab_mut(tab_id);
        let SessionsProfile::Invalid(error) = &tab.agents_view.ssh_profile else {
            return false;
        };
        let error = error.clone();
        tracing::warn!(target: "ssh_sessions", %error, "SSH profile cannot provide a session history source");
        self.cancel_ssh_sessions_fetch(tab_id);
        let tab = self.tab_mut(tab_id);
        tab.agents_view.ssh_error = Some(error);
        true
    }

    pub(super) fn refresh_profile_sessions_sources(&mut self) {
        let tabs: Vec<_> = self
            .tab_sessions
            .iter()
            .filter(|(_, tab)| matches!(tab.agents_view.ssh_profile, SessionsProfile::Ssh(_)))
            .map(|(id, tab)| (id.clone(), tab.agents_view.ssh_source.clone()))
            .collect();
        for (tab_id, previous) in tabs {
            self.apply_profile_sessions_source(&tab_id);
            let tab = self.tab_mut(&tab_id);
            if tab.agents_view.ssh_source != previous
                && tab.current_view == View::Agents
                && tab.agents_view.snapshot.is_some()
            {
                self.schedule_agents_refetch_for_tab(&tab_id);
            }
        }
    }
}
