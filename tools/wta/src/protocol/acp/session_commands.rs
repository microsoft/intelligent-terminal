use agent_client_protocol::schema::v1::{AvailableCommand, AvailableCommandInput};
use std::collections::HashSet;

use crate::app_contracts::{AcpSessionCommand, CompletionBehavior};

pub(crate) fn normalize(commands: &[AvailableCommand]) -> Vec<AcpSessionCommand> {
    let mut seen = HashSet::new();
    commands
        .iter()
        .filter_map(|command| {
            let name = command.name.trim().trim_start_matches('/');
            if name.is_empty()
                || name.chars().any(char::is_whitespace)
                || !seen.insert(name.to_ascii_lowercase())
            {
                return None;
            }
            let (input_hint, completion_behavior) = match command.input.as_ref() {
                Some(AvailableCommandInput::Unstructured(input)) => (
                    Some(input.hint.trim().to_string()),
                    CompletionBehavior::OptionalFreeText,
                ),
                _ => (None, CompletionBehavior::ExecuteImmediately),
            };
            Some(AcpSessionCommand {
                name: name.to_string(),
                description: command.description.trim().to_string(),
                input_hint,
                completion_behavior,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_preserves_order_and_drops_unusable_duplicates() {
        let commands = vec![
            AvailableCommand::new(" plan ", " Build a plan "),
            AvailableCommand::new("/review", "Review changes"),
            AvailableCommand::new("explain", "Explain code").input(
                AvailableCommandInput::Unstructured(
                    agent_client_protocol::schema::v1::UnstructuredCommandInput::new(""),
                ),
            ),
            AvailableCommand::new("PLAN", "Duplicate"),
            AvailableCommand::new("two words", "Unaddressable"),
            AvailableCommand::new("", "Empty"),
        ];

        assert_eq!(
            normalize(&commands),
            vec![
                AcpSessionCommand {
                    name: "plan".into(),
                    description: "Build a plan".into(),
                    input_hint: None,
                    completion_behavior: CompletionBehavior::ExecuteImmediately,
                },
                AcpSessionCommand {
                    name: "review".into(),
                    description: "Review changes".into(),
                    input_hint: None,
                    completion_behavior: CompletionBehavior::ExecuteImmediately,
                },
                AcpSessionCommand {
                    name: "explain".into(),
                    description: "Explain code".into(),
                    input_hint: Some(String::new()),
                    completion_behavior: CompletionBehavior::OptionalFreeText,
                },
            ]
        );
    }
}
