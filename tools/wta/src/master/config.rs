#[derive(Debug)]
pub(crate) struct MasterConfig {
    pub(crate) agent: String,
    pub(crate) agent_id: Option<String>,
    pub(crate) allowed_agent_ids: Vec<String>,
    pub(crate) session_management_enabled: bool,
}

pub(crate) fn parse_custom_agent_commands(
    raw: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let commands: std::collections::HashMap<String, String> = serde_json::from_str(raw)
        .map_err(|_| "custom agent commands must be a JSON object of strings".to_string())?;
    let mut normalized = std::collections::HashMap::new();
    for (id, command) in commands {
        let id = id.to_ascii_lowercase();
        if !id.strip_prefix("custom:").is_some_and(|name| {
            !name.is_empty() && name.trim() == name && !id.chars().any(char::is_control)
        }) || command.trim().is_empty()
            || command.contains('\0')
        {
            return Err("invalid custom agent identity or command".to_string());
        }
        if normalized.insert(id, command).is_some() {
            return Err("duplicate custom agent identity".to_string());
        }
    }
    Ok(normalized)
}

pub(crate) fn parse_custom_acp_agent_catalog(
    catalog: &serde_json::Value,
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    parse_custom_agent_commands(&serde_json::to_string(catalog)?).map_err(anyhow::Error::msg)
}
