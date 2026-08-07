use crate::agent_source::AgentSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableAgent {
    pub id: String,
    pub display_name: String,
    pub source: AgentSource,
}
