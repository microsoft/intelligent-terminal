//! Per-tab agent-pane chat-history persistence.
//!
//! Each helper proactively writes its owner tab's rendered chat UI history
//! (the `CompletedTurn` rows the user actually saw — prompt titles,
//! recommendation cards, executed markers) plus its ACP `session_id` to a
//! stable per-tab file:
//!
//! ```text
//! …\LocalCache\Local\IntelligentTerminal\agent-pane-history\{ownerTabId}.json
//! ```
//!
//! The Eternal-Terminal workspace save (C++ side,
//! `IntelligentTerminal::AgentPaneHistoryDir()`) copies the selected tabs'
//! files into the workspace snapshot so `/restore-ws` can rehydrate the exact
//! UI (via `--initial-chat-history`) instead of relying on the agent CLI's
//! plain-text `session/load` replay. Writing proactively (on every turn end)
//! means save never has to round-trip to the owning helper — it just reads
//! files — which also generalizes cleanly to multi-tab workspaces.

use std::path::PathBuf;

use crate::app::CompletedTurn;

/// On-disk shape: the tab's ACP session id (for `session/load` on restore) plus
/// the rendered turn history (for exact UI rehydration).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatHistoryFile {
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub completed_turns: Vec<CompletedTurn>,
}

/// `…\IntelligentTerminal\agent-pane-history` (packaged local/cache root) or the
/// bare fallback when unpackaged. `None` when the runtime root can't be
/// resolved.
pub fn dir() -> Option<PathBuf> {
    crate::runtime_paths::intelligent_terminal_local_root().map(|r| r.join("agent-pane-history"))
}

/// Absolute path of a tab's history file. The WT tab StableId is a braced
/// GUID (`{...}`); braces are stripped for a cleaner filename. The C++ save
/// side strips identically so both agree.
pub fn path_for(tab_id: &str) -> Option<PathBuf> {
    let stem: String = tab_id.chars().filter(|c| *c != '{' && *c != '}').collect();
    dir().map(|d| d.join(format!("{stem}.json")))
}

/// Serialize and write a tab's history file (creating the dir). Best-effort:
/// returns the error so callers can log but need not fail the turn.
pub fn write(tab_id: &str, history: &ChatHistoryFile) -> std::io::Result<()> {
    let Some(path) = path_for(tab_id) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "agent-pane-history dir unavailable",
        ));
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(history)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)
}

/// Read + parse a history file from an explicit path (used on restore, where
/// C++ passes the workspace-local copy via `--initial-chat-history`).
pub fn read_path(path: &std::path::Path) -> Option<ChatHistoryFile> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<ChatHistoryFile>(&bytes).ok()
}
