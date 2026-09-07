#[derive(Debug)]
pub(crate) struct MasterConfig {
    pub(crate) agent: String,
    pub(crate) agent_id: Option<String>,
    pub(crate) allowed_agent_ids: Vec<String>,
    pub(crate) session_management_enabled: bool,
}
