#[derive(Debug)]
pub(crate) struct MasterConfig {
    pub(crate) agent: String,
    pub(crate) agent_id: Option<String>,
    pub(crate) allowed_agent_ids: Vec<String>,
    pub(crate) acp_model: Option<String>,
    pub(crate) custom_model_selection: Option<String>,
    pub(crate) cloud_models: Option<String>,
}
