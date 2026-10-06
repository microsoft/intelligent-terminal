#[derive(Debug)]
pub(crate) struct HelperConfig {
    pub(crate) prompt: Option<String>,
    pub(crate) agent: String,
    pub(crate) agent_id: Option<String>,
    pub(crate) agent_source: Option<String>,
    pub(crate) agent_wsl_distro: Option<String>,
    pub(crate) agent_source_cwd: Option<String>,
    pub(crate) allowed_agent_ids: Vec<String>,
    pub(crate) initial_auth_agent: Option<String>,
    pub(crate) acp_model: Option<String>,
    pub(crate) follows_global_acp_model: bool,
    pub(crate) custom_model_selection: Option<String>,
    pub(crate) custom_models: Option<String>,
    pub(crate) cloud_models: Option<String>,
    pub(crate) delegate_agent: Option<String>,
    pub(crate) delegate_agent_id: Option<String>,
    pub(crate) delegate_model: Option<String>,
    pub(crate) no_autofix: bool,
    pub(crate) autofix_policy_state: crate::telemetry::AutoFixPolicyState,
    pub(crate) yolo_mode: bool,
    pub(crate) yolo_policy_blocked: bool,
    pub(crate) setup: Option<String>,
    pub(crate) initial_view: InitialView,
    pub(crate) initial_pane_position: Option<String>,
    pub(crate) owner_tab_id: Option<String>,
    pub(crate) owner_window_id: Option<String>,
    pub(crate) initial_load_session_id: Option<String>,
    pub(crate) initial_load_cwd: Option<String>,
    pub(crate) initial_yolo_control_owner: Option<crate::app_contracts::YoloControlOwner>,
    pub(crate) start_stashed: bool,
}

impl HelperConfig {
    pub(crate) fn resolve_delegate_runtime(
        &self,
    ) -> anyhow::Result<crate::coordinator::DelegateAgentRuntime> {
        let delegate_command = self
            .delegate_agent
            .as_deref()
            .filter(|command| !command.trim().is_empty());
        crate::coordinator::resolve_delegate_runtime_with_provider(
            delegate_command,
            Some(&self.agent),
            self.delegate_model.as_deref(),
            self.delegate_agent_id.as_deref().or_else(|| {
                delegate_command
                    .is_none()
                    .then_some(self.agent_id.as_deref())
                    .flatten()
            }),
        )
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum InitialView {
    Chat,
    Sessions,
}
