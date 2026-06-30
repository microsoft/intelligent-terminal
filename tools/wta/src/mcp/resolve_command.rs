//! `resolve_command` MCP tool — the pull-mode counterpart of the autofix
//! near-match injection. The agent calls it when it doesn't recognize a command
//! the user typed/asked about: it returns the closest *real* commands that
//! exist in this PowerShell environment (PATH programs, scripts, cmdlets,
//! functions, aliases), or an empty list if nothing close. Same core
//! ([`crate::command_recall`]) autofix uses in-process, so behavior matches.

use async_trait::async_trait;

use super::Tool;

pub struct ResolveCommand;

#[async_trait]
impl Tool for ResolveCommand {
    fn name(&self) -> &'static str {
        "resolve_command"
    }

    fn description(&self) -> &'static str {
        "Resolve a possibly-mistyped command to the closest real commands on this \
         machine (PowerShell). Use when a command isn't recognized to suggest what \
         the user likely meant. Returns near-matches, closest first; empty if none."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "token": { "type": "string", "description": "The command name the user typed (no args/path)." },
                "shell": { "type": "string", "description": "Optional shell exe; defaults to pwsh. PowerShell only in v1." }
            },
            "required": ["token"]
        })
    }

    async fn call(&self, args: &serde_json::Value) -> Result<String, String> {
        let token = args
            .get("token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("missing required 'token'")?;
        let shell = args
            .get("shell")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("pwsh.exe");
        if !crate::command_recall::is_powershell(shell) {
            return Ok(serde_json::json!({ "token": token, "matches": [], "note": "non-PowerShell shells unsupported in v1" }).to_string());
        }
        let matches = crate::command_recall::powershell_near_matches(shell, token)
            .await
            .unwrap_or_default();
        Ok(serde_json::json!({ "token": token, "matches": matches }).to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_missing_token() {
        assert!(ResolveCommand.call(&serde_json::json!({})).await.is_err());
    }

    #[tokio::test]
    async fn non_powershell_returns_empty() {
        let out = ResolveCommand
            .call(&serde_json::json!({ "token": "gti", "shell": "bash" }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["matches"].as_array().unwrap().len(), 0);
    }
}
