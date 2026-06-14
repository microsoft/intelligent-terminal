//! Map a changed session-file path under one of the four watched roots to the
//! CLI, session key, and (where the path encodes it) the session's cwd.
//!
//! Path → identity, verified against real layouts:
//!   Copilot : ~/.copilot/session-state/<UUID>/events.jsonl        key=<UUID>
//!   Claude  : ~/.claude/projects/<encoded-cwd>/<UUID>.jsonl       key=<UUID>, cwd=decode(<encoded-cwd>)
//!   Codex   : ~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<UUID>.jsonl  key=<UUID>
//!   Gemini  : ~/.gemini/tmp/<slug>/chats/session-*.jsonl          key=<file-stem>

use crate::agent_sessions::CliSource;
use crate::history_loader::decode_claude_cwd;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub cli: CliSource,
    pub key: String,
    /// Path-encoded cwd when available (Claude only, today).
    pub cwd: Option<PathBuf>,
}

/// Classify a changed path. Returns `None` for paths that are not a
/// recognized session file (e.g. a sibling `workspace.yaml`).
pub fn identify(path: &Path) -> Option<Discovered> {
    let name = path.file_name()?.to_str()?;

    // Copilot: .../session-state/<UUID>/events.jsonl
    if name == "events.jsonl" {
        let key = path.parent()?.file_name()?.to_str()?.to_string();
        if path.components().any(|c| c.as_os_str() == "session-state") {
            return Some(Discovered {
                cli: CliSource::Copilot,
                key,
                cwd: None,
            });
        }
    }

    // Codex: rollout-<ts>-<UUID>.jsonl
    if name.starts_with("rollout-") && name.ends_with(".jsonl") {
        // rollout-<iso-ts>-<uuid>.jsonl — the UUID is the last 5
        // hyphen-delimited groups (8-4-4-4-12); the ISO timestamp before it
        // also contains hyphens, so we can't just split on the last '-'.
        let stem = name.trim_end_matches(".jsonl");
        let parts: Vec<&str> = stem.split('-').collect();
        let key = if parts.len() >= 5 {
            parts[parts.len() - 5..].join("-")
        } else {
            // Unexpected shape — fall back to the whole stem so we still
            // produce *some* stable key rather than dropping the session.
            stem.to_string()
        };
        return Some(Discovered {
            cli: CliSource::Codex,
            key,
            cwd: None,
        });
    }

    // Gemini: .../tmp/<slug>/chats/session-*.jsonl
    if name.starts_with("session-")
        && name.ends_with(".jsonl")
        && path.components().any(|c| c.as_os_str() == "chats")
    {
        let key = name.trim_end_matches(".jsonl").to_string();
        return Some(Discovered {
            cli: CliSource::Gemini,
            key,
            cwd: None,
        });
    }

    // Claude: .../projects/<encoded-cwd>/<UUID>.jsonl
    if name.ends_with(".jsonl") && path.components().any(|c| c.as_os_str() == "projects") {
        let key = name.trim_end_matches(".jsonl").to_string();
        let cwd = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|d| d.to_str())
            .map(decode_claude_cwd);
        return Some(Discovered {
            cli: CliSource::Claude,
            key,
            cwd,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copilot_path() {
        let p = Path::new(r"C:\Users\u\.copilot\session-state\abc-123\events.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Copilot);
        assert_eq!(d.key, "abc-123");
    }

    #[test]
    fn codex_path_uses_full_uuid_key() {
        let p = Path::new(r"C:/Users/u/.codex/sessions/2026/06/08/rollout-2026-06-08T21-29-13-019ea76c-4c47-7da1-9c47-5f814a9e3640.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Codex);
        assert_eq!(d.key, "019ea76c-4c47-7da1-9c47-5f814a9e3640");
    }

    #[test]
    fn gemini_path() {
        let p = Path::new(r"C:\Users\u\.gemini\tmp\slug\chats\session-2026-06-08T14-01-d6ce.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Gemini);
        assert_eq!(d.key, "session-2026-06-08T14-01-d6ce");
    }

    #[test]
    fn claude_path_decodes_cwd() {
        let p = Path::new(r"C:\Users\u\.claude\projects\C--Users-u\aaaa-bbbb.jsonl");
        let d = identify(p).unwrap();
        assert_eq!(d.cli, CliSource::Claude);
        assert_eq!(d.key, "aaaa-bbbb");
        assert!(d.cwd.is_some());
    }

    #[test]
    fn unrelated_path_is_none() {
        assert!(identify(Path::new(
            r"C:\Users\u\.copilot\session-state\abc\workspace.yaml"
        ))
        .is_none());
    }
}
