/// Status of a single preflight check.
#[derive(Debug, Clone, PartialEq)]
pub enum CheckStatus {
    Checking,
    Passed,
    Failed(String),
    Skipped,
}

/// Result of all preflight checks for an agent.
#[derive(Debug, Clone)]
pub struct PreflightResult {
    pub agent_id: String,
    pub display_name: String,
    pub cli_status: CheckStatus,
    pub cli_path: Option<String>,
    pub auth_status: CheckStatus,
    pub install_hint: String,
    pub install_url: String,
    pub auth_hint: String,
}

impl PreflightResult {
    pub fn all_passed(&self) -> bool {
        self.cli_status == CheckStatus::Passed
            && matches!(self.auth_status, CheckStatus::Passed | CheckStatus::Skipped)
    }

    /// Synthesize a passed result for a custom or unknown agent command.
    pub fn passed_for_custom_agent(canonical_id: &str) -> Self {
        let display_name = canonical_id
            .strip_prefix("custom:")
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| canonical_id.to_string());
        Self {
            agent_id: canonical_id.to_string(),
            display_name,
            cli_status: CheckStatus::Passed,
            cli_path: None,
            auth_status: CheckStatus::Skipped,
            install_hint: String::new(),
            install_url: String::new(),
            auth_hint: String::new(),
        }
    }
}
