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
//! The Eternal-Terminal workspace save still reads these files to learn the
//! ACP `session_id` for `session/load`. The chat UI itself is rebuilt from the
//! agent CLI's replay, so the rendered turn snapshot is transitional data until
//! the session-id channel moves to events.

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
