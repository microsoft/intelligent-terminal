use agent_client_protocol as acp;

use crate::app_contracts::{AcpSessionConfigOption, AcpSessionConfigValue};

pub(crate) fn select_options(
    options: &[acp::schema::v1::SessionConfigOption],
) -> Vec<AcpSessionConfigOption> {
    options.iter().filter_map(select_option).collect()
}

fn select_option(option: &acp::schema::v1::SessionConfigOption) -> Option<AcpSessionConfigOption> {
    let acp::schema::v1::SessionConfigKind::Select(select) = &option.kind else {
        return None;
    };

    let values = match &select.options {
        acp::schema::v1::SessionConfigSelectOptions::Ungrouped(values) => {
            values.iter().map(normalize_value).collect()
        }
        acp::schema::v1::SessionConfigSelectOptions::Grouped(groups) => groups
            .iter()
            .flat_map(|group| group.options.iter().map(normalize_value))
            .collect(),
        _ => return None,
    };

    Some(AcpSessionConfigOption {
        id: option.id.0.to_string(),
        name: option.name.clone(),
        description: option.description.clone(),
        category: option
            .category
            .as_ref()
            .and_then(|category| serde_json::to_value(category).ok())
            .and_then(|category| category.as_str().map(str::to_owned)),
        current_value: select.current_value.0.to_string(),
        values,
        native_yolo: false,
    })
}

fn normalize_value(value: &acp::schema::v1::SessionConfigSelectOption) -> AcpSessionConfigValue {
    AcpSessionConfigValue {
        id: value.value.0.to_string(),
        name: value.name.clone(),
        description: value.description.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_order_categories_and_current_values() {
        let response: acp::schema::v1::NewSessionResponse =
            serde_json::from_value(serde_json::json!({
                "sessionId": "session-1",
                "configOptions": [
                    {
                        "id": "mode",
                        "name": "Mode",
                        "description": "Controls agent behavior",
                        "category": "mode",
                        "type": "select",
                        "currentValue": "code",
                        "options": [
                            {"value": "ask", "name": "Ask"},
                            {"value": "code", "name": "Code", "description": "Write code"}
                        ]
                    },
                    {
                        "id": "reasoning",
                        "name": "Reasoning",
                        "category": "thought_level",
                        "type": "select",
                        "currentValue": "high",
                        "options": [{"value": "high", "name": "High"}]
                    }
                ]
            }))
            .expect("valid response");

        let options = select_options(response.config_options.as_deref().expect("config options"));

        assert_eq!(options.len(), 2);
        assert_eq!(options[0].id, "mode");
        assert_eq!(options[0].category.as_deref(), Some("mode"));
        assert_eq!(options[0].current_value_name(), "Code");
        assert_eq!(
            options[0].values[1].description.as_deref(),
            Some("Write code")
        );
        assert_eq!(options[1].category.as_deref(), Some("thought_level"));
    }

    #[test]
    fn ignores_unsupported_option_kinds() {
        let response: acp::schema::v1::NewSessionResponse =
            serde_json::from_value(serde_json::json!({
                "sessionId": "session-1",
                "configOptions": [{
                    "id": "enabled",
                    "name": "Enabled",
                    "type": "boolean",
                    "currentValue": true
                }]
            }))
            .expect("valid response");

        assert!(select_options(response.config_options.as_deref().unwrap()).is_empty());
    }
}
