//! `resolve_command` MCP tool — the pull-mode, profile-aware command
//! identifier. The agent calls it when the user asks what a command is / how to
//! use it, or names a command the agent doesn't recognize. Every response has
//! the same `{token, status, …}` shape, grounded in the user's real
//! (profile-loaded) PowerShell environment:
//!
//! - `status:"exists"` → `resolutions` (each command type + resolved target,
//!   e.g. an alias → its target). This is the issue #286 answer: a
//!   profile-defined alias like `which` → `where.exe` that the agent's own
//!   `-NoProfile` probe would miss.
//! - `status:"not_found"` → `matches`, the closest real commands (typo "did you
//!   mean", issue #287), or empty if nothing is close.
//! - `status:"indeterminate"` → couldn't verify (profile probe timed out /
//!   failed); the agent must not assume the command is missing.
//! - `status:"unsupported"` → non-PowerShell shell (v1).
//!
//! Both grounded paths share the [`crate::command_recall`] core autofix uses.

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
         aliases/functions that a `-NoProfile` probe misses. Always returns a \
         `status`: `exists` (with `resolutions` — each type + resolved target, \
         e.g. an alias -> its target), `not_found` (with `matches` — the closest \
         real commands, closest first, or empty), `indeterminate` (couldn't \
         verify, e.g. the profile probe timed out — do NOT assume it's missing; \
         fall back to your own probe), or `unsupported` (non-PowerShell shell)."
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
        use crate::command_recall::ResolveOutcome;

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
            return Ok(serde_json::json!({
                "token": token,
                "status": "unsupported",
                "note": "non-PowerShell shells unsupported in v1",
            })
            .to_string());
        }

        // Every branch returns the same `{token, status, …}` shape so callers
        // (and the agent) never have to special-case a path.
        match crate::command_recall::powershell_resolve(shell, token).await {
            // Existing command → what it resolves to (the "what is X" answer the
            // agent's own -NoProfile probe can't give).
            ResolveOutcome::Resolved(resolutions) => {
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
                Ok(serde_json::json!({
                    "token": token,
                    "status": "exists",
                    "resolutions": resolutions,
                })
                .to_string())
            }
            // Ran cleanly, resolves to nothing → closest real commands ("did you mean").
            ResolveOutcome::NotFound => {
                let matches = crate::command_recall::powershell_near_matches(shell, token)
                    .await
                    .unwrap_or_default();
                Ok(serde_json::json!({
                    "token": token,
                    "status": "not_found",
                    "matches": matches,
                })
                .to_string())
            }
            // Timeout / spawn / IO error → don't claim it's missing.
            ResolveOutcome::Indeterminate => Ok(serde_json::json!({
                "token": token,
                "status": "indeterminate",
                "note": "could not verify on this machine (the profile probe timed out or failed); fall back to your own read-only probe",
            })
            .to_string()),
        }
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
    async fn non_powershell_returns_unsupported() {
        let out = ResolveCommand
            .call(&serde_json::json!({ "token": "gti", "shell": "bash" }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "unsupported", "got {v}");
    }

    /// Windows-only end-to-end: an always-present core cmdlet resolves to
    /// `status:"exists"` with its type, and a nonsense token resolves to
    /// `status:"not_found"`. Uses `Get-ChildItem` (a core cmdlet that a profile
    /// can't remove) rather than a built-in alias like `gci`, so it doesn't
    /// depend on the contributor's environment. Skips when no PowerShell host.
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

        // `Get-ChildItem` is present in every PowerShell host, profile or not.
        let out = ResolveCommand
            .call(&serde_json::json!({ "token": "Get-ChildItem", "shell": shell }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        // A slow/hanging profile can legitimately time out → indeterminate (part
        // of the tool contract); skip rather than fail on such machines.
        if v["status"] == "indeterminate" {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(v["status"], "exists", "got {v}");
        let res = v["resolutions"].as_array().expect("resolutions array");
        assert!(
            res.iter().any(|r| r["type"] == "Cmdlet" && r["name"] == "Get-ChildItem"),
            "expected Get-ChildItem as a Cmdlet, got {v}"
        );

        // A token that resolves to nothing → status:"not_found".
        let out = ResolveCommand
            .call(&serde_json::json!({ "token": "no-such-command", "shell": shell }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        if v["status"] == "indeterminate" {
            eprintln!("resolve was indeterminate (slow profile?); skipping");
            return;
        }
        assert_eq!(v["status"], "not_found", "got {v}");
        assert!(v["matches"].is_array(), "expected a matches array, got {v}");
    }
}
