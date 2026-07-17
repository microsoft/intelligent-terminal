//! Durable shell-session index reader for the `/shell-sessions` picker.
//!
//! Windows Terminal (C++) writes `shell-sessions.json` into the shared
//! IntelligentTerminal `LocalState` directory whenever a tab is closed (see
//! `TerminalPage::_WriteShellSessionsIndexEntry`). This module reads that
//! lightweight sidecar so the agent-pane TUI can render the picker without
//! having to parse WT's `state.json` layout format.
//!
//! The authoritative layout + scrollback live on the C++ side; restoring is a
//! one-way `restore_shell_session` protocol event keyed by the session name.

use serde::Deserialize;

use crate::runtime_paths::intelligent_terminal_root;

/// One saved shell session, as written by the C++ side.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ShellSessionEntry {
    /// The closed tab's title — this is both the display label and the key the
    /// C++ side looks up in `ApplicationState::GetShellSession`.
    pub name: String,
    /// Working directory captured at save time, for display only. May be empty
    /// when the shell never reported one (no OSC 9;9 shell integration).
    #[serde(default)]
    pub cwd: String,
    /// Unix seconds when the session was saved. Used to sort newest-first.
    #[serde(default)]
    pub saved_at: i64,
}

/// On-disk shape of `shell-sessions.json`.
#[derive(Debug, Clone, Deserialize, Default)]
struct ShellSessionsIndex {
    #[serde(default)]
    sessions: Vec<ShellSessionEntry>,
}

/// The index file's name inside `intelligent_terminal_root()`.
const INDEX_FILE_NAME: &str = "shell-sessions.json";

/// Read the saved shell sessions, most-recently-saved first.
///
/// Returns an empty vector when the index file is missing or unreadable — an
/// absent file just means the user hasn't closed any tabs yet, so callers
/// surface a "no saved sessions" message rather than an error.
pub fn load() -> Vec<ShellSessionEntry> {
    let Some(path) = intelligent_terminal_root().map(|root| root.join(INDEX_FILE_NAME)) else {
        return Vec::new();
    };

    let contents = match std::fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(err) => {
            // `NotFound` is the expected "nothing saved yet" case; only louder
            // errors (permissions, etc.) are worth a log line.
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(target: "shell_sessions", %err, "failed to read shell-sessions.json");
            }
            return Vec::new();
        }
    };

    let mut index: ShellSessionsIndex = match serde_json::from_str(&contents) {
        Ok(index) => index,
        Err(err) => {
            tracing::warn!(target: "shell_sessions", %err, "failed to parse shell-sessions.json");
            return Vec::new();
        }
    };

    // Newest first so the picker's top row is the most recently closed tab.
    index.sessions.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    index.sessions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_sorts_newest_first() {
        let json = r#"{
            "sessions": [
                { "name": "old", "cwd": "C:\\a", "saved_at": 100 },
                { "name": "new", "cwd": "C:\\b", "saved_at": 200 }
            ]
        }"#;
        let mut index: ShellSessionsIndex = serde_json::from_str(json).unwrap();
        index.sessions.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
        assert_eq!(index.sessions[0].name, "new");
        assert_eq!(index.sessions[1].name, "old");
        assert_eq!(index.sessions[0].cwd, "C:\\b");
    }

    #[test]
    fn missing_optional_fields_default() {
        let json = r#"{ "sessions": [ { "name": "only-name" } ] }"#;
        let index: ShellSessionsIndex = serde_json::from_str(json).unwrap();
        assert_eq!(index.sessions[0].name, "only-name");
        assert_eq!(index.sessions[0].cwd, "");
        assert_eq!(index.sessions[0].saved_at, 0);
    }

    #[test]
    fn empty_or_missing_sessions_is_empty() {
        let index: ShellSessionsIndex = serde_json::from_str("{}").unwrap();
        assert!(index.sessions.is_empty());
    }
}
