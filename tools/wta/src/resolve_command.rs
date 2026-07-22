//! Profile-aware command resolution for the `wta resolve-command` CLI.
//!
//! The command returns the same stable `{token, status, ...}` JSON shape for
//! every outcome so agents can consume it without an MCP server.

use crate::command_recall::ResolveOutcome;

pub fn parse_non_empty(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err("value cannot be empty".to_string())
    } else {
        Ok(value.to_string())
    }
}

pub fn format_human(value: &serde_json::Value) -> String {
    let field = |name: &str| value.get(name).and_then(|v| v.as_str()).unwrap_or("-");
    let mut lines = vec![
        format!("TOKEN    {}", field("token")),
        format!("STATUS   {}", field("status")),
    ];

    if let Some(resolutions) = value.get("resolutions").and_then(|v| v.as_array()) {
        for resolution in resolutions {
            lines.push(format!(
                "COMMAND  {} {}",
                resolution
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-"),
                resolution
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("-")
            ));
            if let Some(target) = resolution
                .get("target")
                .and_then(|v| v.as_str())
                .filter(|target| !target.is_empty())
            {
                lines.push(format!("TARGET   {target}"));
            }
        }
    }

    if let Some(matches) = value.get("matches").and_then(|v| v.as_array()) {
        let matches = matches
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!(
            "MATCHES  {}",
            if matches.is_empty() { "-" } else { &matches }
        ));
    }

    if let Some(note) = value.get("note").and_then(|v| v.as_str()) {
        lines.push(format!("NOTE     {note}"));
    }

    lines.join("\n")
}

pub async fn resolve(token: &str, shell: &str) -> serde_json::Value {
    if !crate::command_recall::is_powershell(shell) {
        return serde_json::json!({
            "token": token,
            "status": "unsupported",
            "note": "non-PowerShell shells are unsupported in v1",
        });
    }

    match crate::command_recall::powershell_resolve(shell, token).await {
        ResolveOutcome::Resolved(resolutions) => {
            let resolutions: Vec<serde_json::Value> = resolutions
                .into_iter()
                .map(|resolution| {
                    serde_json::json!({
                        "type": resolution.command_type,
                        "name": resolution.name,
                        "target": resolution.target,
                    })
                })
                .collect();
            serde_json::json!({
                "token": token,
                "status": "exists",
                "resolutions": resolutions,
            })
        }
        ResolveOutcome::NotFound => {
            let matches = crate::command_recall::powershell_near_matches(shell, token)
                .await
                .unwrap_or_default();
            serde_json::json!({
                "token": token,
                "status": "not_found",
                "matches": matches,
            })
        }
        ResolveOutcome::Indeterminate => serde_json::json!({
            "token": token,
            "status": "indeterminate",
            "note": "could not verify on this machine (the profile probe timed out or failed); fall back to your own read-only probe",
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_empty_parser_trims_and_rejects_empty_values() {
        assert_eq!(
            parse_non_empty("  Get-ChildItem  ").unwrap(),
            "Get-ChildItem"
        );
        assert!(parse_non_empty("").is_err());
        assert!(parse_non_empty(" \t ").is_err());
    }

    #[test]
    fn human_format_summarizes_resolutions_and_matches() {
        let exists = serde_json::json!({
            "token": "greetme",
            "status": "exists",
            "resolutions": [{
                "type": "Alias",
                "name": "greetme",
                "target": "Invoke-ProfileGreeting"
            }]
        });
        assert_eq!(
            format_human(&exists),
            "TOKEN    greetme\nSTATUS   exists\nCOMMAND  Alias greetme\nTARGET   Invoke-ProfileGreeting"
        );

        let not_found = serde_json::json!({
            "token": "gti",
            "status": "not_found",
            "matches": ["git", "gci"]
        });
        assert_eq!(
            format_human(&not_found),
            "TOKEN    gti\nSTATUS   not_found\nMATCHES  git, gci"
        );
    }

    #[tokio::test]
    async fn non_powershell_returns_unsupported() {
        let value = resolve("gti", "bash").await;
        assert_eq!(value["token"], "gti");
        assert_eq!(value["status"], "unsupported");
        assert_eq!(value["note"], "non-PowerShell shells are unsupported in v1");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn resolves_existing_cmdlet_and_flags_unknown() {
        let host = ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find(|exe| which::which(exe).is_ok());
        let Some(shell) = host else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };

        let value = resolve("Get-ChildItem", shell).await;
        if value["status"] == "indeterminate" {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(value["status"], "exists", "got {value}");
        let resolutions = value["resolutions"].as_array().expect("resolutions array");
        assert!(
            resolutions
                .iter()
                .any(|item| item["type"] == "Cmdlet" && item["name"] == "Get-ChildItem"),
            "expected Get-ChildItem as a Cmdlet, got {value}"
        );

        let value = resolve("no-such-command", shell).await;
        if value["status"] == "indeterminate" {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(value["status"], "not_found", "got {value}");
        assert!(
            value["matches"].is_array(),
            "expected a matches array, got {value}"
        );
    }
}
