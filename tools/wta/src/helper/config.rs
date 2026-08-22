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
    pub(crate) delegate_model: Option<String>,
    pub(crate) no_autofix: bool,
    pub(crate) render_agent_markdown: bool,
    pub(crate) setup: Option<String>,
    pub(crate) initial_view: InitialView,
    pub(crate) owner_tab_id: Option<String>,
    pub(crate) owner_window_id: Option<String>,
    pub(crate) initial_load_session_id: Option<String>,
    pub(crate) initial_load_cwd: Option<String>,
    pub(crate) start_stashed: bool,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum InitialView {
    Chat,
    Sessions,
}
