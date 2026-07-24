use crate::proposal_channel::ProposalChannel;
use crate::terminal_action_proposal::MAX_PAYLOAD_BYTES;

const PREFIX: &str = r#"& "$env:WTA_CLI_PATH" propose-terminal-actions --channel "#;
const PAYLOAD_MARKER: &str = " --payload-json ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposalInvocation {
    pub channel: ProposalChannel,
    pub payload: String,
}

pub fn render(channel: &ProposalChannel, payload: &str) -> Result<String, &'static str> {
    validate_payload(payload)?;
    Ok(format!(
        "{PREFIX}{channel}{PAYLOAD_MARKER}'{}'",
        payload.replace('\'', "''")
    ))
}

pub fn parse(command: &str) -> Result<ProposalInvocation, &'static str> {
    if command.contains('\r') || command.contains('\n') {
        return Err("proposal command must be one line");
    }
    let rest = command
        .strip_prefix(PREFIX)
        .ok_or("proposal command does not use WTA_CLI_PATH")?;
    let (channel_text, payload_expression) = rest
        .split_once(PAYLOAD_MARKER)
        .ok_or("proposal command is missing --payload-json")?;
    let channel = channel_text
        .parse::<ProposalChannel>()
        .map_err(|_| "proposal channel is malformed")?;
    let payload = decode_single_quoted(payload_expression)
        .ok_or("proposal payload must be one PowerShell single-quoted argument")?;
    validate_payload(&payload)?;
    let invocation = ProposalInvocation { channel, payload };
    if render(&invocation.channel, &invocation.payload)? != command {
        return Err("proposal command is not canonical");
    }
    Ok(invocation)
}

fn validate_payload(payload: &str) -> Result<(), &'static str> {
    if payload.is_empty() || payload.len() > MAX_PAYLOAD_BYTES {
        return Err("proposal payload is empty or too large");
    }
    let value: serde_json::Value =
        serde_json::from_str(payload).map_err(|_| "proposal payload is invalid JSON")?;
    let compact =
        serde_json::to_string(&value).map_err(|_| "proposal payload could not be encoded")?;
    if compact != payload {
        return Err("proposal payload must be compact JSON");
    }
    Ok(())
}

fn decode_single_quoted(expression: &str) -> Option<String> {
    let body = expression.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut decoded = String::with_capacity(body.len());
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if chars.next() != Some('\'') {
                return None;
            }
            decoded.push('\'');
        } else {
            decoded.push(ch);
        }
    }
    Some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proposal_channel::ProposalChannelManager;

    fn payload() -> &'static str {
        r#"{"schema_version":1,"origin":"terminal_agent","recommended_choice":1,"choices":[{"choice":1,"title":"Run user's test","rationale":"","actions":[{"type":"send","input":"cargo test"}]}]}"#
    }

    #[test]
    fn canonical_command_round_trips_apostrophes() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let command = render(&channel, payload()).unwrap();
        assert!(command.contains("user''s test"));
        let parsed = parse(&command).unwrap();
        assert_eq!(parsed.channel, channel);
        assert_eq!(parsed.payload, payload());
    }

    #[test]
    fn rejects_former_pipe_and_here_string_shapes() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        assert!(parse(&format!(
            "'{}' | & \"$env:WTA_CLI_PATH\" propose-terminal-actions --channel {channel}",
            payload()
        ))
        .is_err());
        assert!(parse(&format!(
            "@'\n{}\n'@ | & \"$env:WTA_CLI_PATH\" propose-terminal-actions --channel {channel}",
            payload()
        ))
        .is_err());
    }

    #[test]
    fn rejects_extra_tokens_and_noncompact_json() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let command = render(&channel, payload()).unwrap();
        assert!(parse(&format!("{command} --extra")).is_err());
        assert!(render(&channel, &payload().replace(",", ", ")).is_err());
    }

    #[test]
    fn rejects_alternate_executable_spelling() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let command = render(&channel, payload())
            .unwrap()
            .replace("$env:WTA_CLI_PATH", "wta.exe");
        assert!(parse(&command).is_err());
    }

    #[test]
    fn defers_proposal_schema_validation_to_helper() {
        let manager = ProposalChannelManager::new();
        let channel = manager.issue("session".into(), 1, None, false).unwrap();
        let payload = r#"{"schema_version":999}"#;
        let command = render(&channel, payload).unwrap();
        assert_eq!(parse(&command).unwrap().payload, payload);
    }
}
