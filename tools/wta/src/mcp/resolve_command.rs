//! `resolve_command` MCP tool — the pull-mode, profile-aware command
//! identifier. The agent calls it when the user asks what a command is / how to
//! use it, or names a command the agent doesn't recognize. Two outcomes, both
//! grounded in the user's real (profile-loaded) PowerShell environment:
//!
//! - **exists** → `exists:true` with `resolutions` (each command type + resolved
//!   target, e.g. an alias → its target). This is the issue #286 answer: a
//!   profile-defined alias like `which` → `where.exe` that the agent's own
//!   `-NoProfile` probe would miss.
//! - **not found** → `exists:false` with `matches`, the closest real commands
//!   (typo "did you mean", issue #287), or empty if nothing is close.
//!
//! Both share the [`crate::command_recall`] core autofix uses in-process.

use async_trait::async_trait;

use super::Tool;

pub struct ResolveCommand;

#[async_trait]
impl Tool for ResolveCommand {
    fn name(&self) -> &'static str {
        "resolve_command"
    }

    fn description(&self) -> &'static str {
        "Identify a command on this machine (PowerShell), profile-aware. Prefer \
         this over running your own `Get-Command`/`Get-Alias` probe when the user \
         asks what a command is, how to use it, or names a command you don't \
         recognize: it loads the user's profile, so it sees profile-defined \
         aliases/functions that a `-NoProfile` probe misses. If the command \
         exists it returns `exists:true` with its resolutions (type + resolved \
         target, e.g. an alias -> its target); if it doesn't, it returns \
         `exists:false` with the closest real commands (`matches`, closest \
         first), or an empty list if nothing is close."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "token": { "type": "string", "description": "The command name to identify (no args/path)." },
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

        // Existing command → report what it resolves to (profile-aware). This is
        // the "what is X" answer the agent's own -NoProfile probe can't give.
        if let Some(resolutions) = crate::command_recall::powershell_resolve(shell, token).await {
            let resolutions: Vec<serde_json::Value> = resolutions
                .into_iter()
                .map(|r| {
                    serde_json::json!({
                        "type": r.command_type,
                        "name": r.name,
                        "target": r.target,
                    })
                })
                .collect();
            return Ok(serde_json::json!({
                "token": token,
                "exists": true,
                "resolutions": resolutions,
            })
            .to_string());
        }

        // Not found → closest real commands (typo "did you mean").
        let matches = crate::command_recall::powershell_near_matches(shell, token)
            .await
            .unwrap_or_default();
        Ok(serde_json::json!({ "token": token, "exists": false, "matches": matches }).to_string())
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

    /// Windows-only end-to-end: a real built-in alias resolves to `exists:true`
    /// with its type, and a nonsense token resolves to `exists:false`. Skips
    /// (no-op) when no PowerShell host is installed.
    #[cfg(windows)]
    #[tokio::test]
    async fn resolves_existing_alias_and_flags_unknown() {
        let host = ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find(|exe| which::which(exe).is_ok());
        let Some(shell) = host else {
            eprintln!("no PowerShell host installed; skipping");
            return;
        };

        // `gci` is a built-in alias for Get-ChildItem in every PowerShell host.
        let out = ResolveCommand
            .call(&serde_json::json!({ "token": "gci", "shell": shell }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exists"], serde_json::Value::Bool(true), "got {v}");
        let res = v["resolutions"].as_array().expect("resolutions array");
        assert!(
            res.iter().any(|r| r["type"] == "Alias" && r["name"] == "gci"),
            "expected gci as an Alias, got {v}"
        );

        // A token that resolves to nothing → exists:false.
        let out = ResolveCommand
            .call(&serde_json::json!({ "token": "wtdefinitelynotacmd", "shell": shell }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["exists"], serde_json::Value::Bool(false), "got {v}");
        assert!(v["matches"].is_array(), "expected a matches array, got {v}");
    }
}
