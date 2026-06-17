//! Agent workspace snapshot and restore.
//!
//! A workspace snapshot captures the agent session state for the currently
//! active pane so it can be restored as a new tab later. This covers:
//!
//!   * The agent session key (e.g. Claude's UUID, Gemini's sessionId)
//!   * The agent CLI source (Claude, Codex, Copilot, Gemini, …)
//!   * The working directory at snapshot time
//!   * A user-supplied or auto-generated name for the snapshot
//!
//! Snapshots are persisted as a JSON file in the user's config directory
//! (`intelligent_terminal_root() / "workspaces.json"`) so they survive
//! Terminal restarts.
//!
//! The `/workspace save [name]` command writes a snapshot; `/workspace restore
//! [name]` lists or restores one. Absent a name, both commands operate on a
//! single default slot named `"default"`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::agent_sessions::CliSource;

// ── Data model ────────────────────────────────────────────────────────────────

/// A point-in-time capture of an agent pane's session state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    /// Snapshot name — the key under which it is stored.
    pub name: String,
    /// Agent session key (what `--resume` / `session/load` consumes).
    pub session_key: String,
    /// Which CLI this session belongs to.
    pub cli_source: CliSource,
    /// Working directory at snapshot time.
    pub working_directory: PathBuf,
    /// RFC 3339 timestamp (seconds-precision).
    pub saved_at: String,
}

/// The persisted store: a map of snapshot name → snapshot.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SnapshotStore {
    snapshots: HashMap<String, WorkspaceSnapshot>,
}

// ── Persistence path ──────────────────────────────────────────────────────────

/// Path to the JSON file that holds all workspace snapshots.
///
/// Returns `None` only when `LOCALAPPDATA` / `APPDATA` is unset — which means
/// we're in an environment where persistence is impossible (e.g. a stripped
/// sandbox). The callers surface an error message in that case.
pub fn snapshot_store_path() -> Option<PathBuf> {
    crate::runtime_paths::intelligent_terminal_root()
        .map(|root| root.join("workspaces.json"))
}

// ── Load / save helpers ───────────────────────────────────────────────────────

fn load_store(path: &PathBuf) -> SnapshotStore {
    let Ok(bytes) = std::fs::read(path) else {
        return SnapshotStore::default();
    };
    serde_json::from_slice(&bytes).unwrap_or_default()
}

fn save_store(path: &PathBuf, store: &SnapshotStore) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_vec_pretty(store)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    std::fs::write(path, json)
}

fn now_rfc3339() -> String {
    // Use std::time for portability; format manually to RFC 3339 seconds precision.
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Format as UTC: YYYY-MM-DDTHH:MM:SSZ
    let s = secs;
    let sec = s % 60;
    let min = (s / 60) % 60;
    let hour = (s / 3600) % 24;
    let days = s / 86400; // days since epoch
    // Compute calendar date from days since 1970-01-01
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Gregorian calendar computation.
    let mut year = 1970u64;
    loop {
        let leap = is_leap(year);
        let days_in_year = if leap { 366 } else { 365 };
        if days < days_in_year {
            break;
        }
        days -= days_in_year;
        year += 1;
    }
    let leap = is_leap(year);
    let month_days: &[u64] = if leap {
        &[31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        &[31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month = 1u64;
    for &md in month_days {
        if days < md {
            break;
        }
        days -= md;
        month += 1;
    }
    (year, month, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Save a workspace snapshot under `name`.
///
/// Returns the saved [`WorkspaceSnapshot`] on success, or an error string
/// suitable for surfacing as a `ChatMessage::System`.
pub fn save(
    name: &str,
    session_key: String,
    cli_source: CliSource,
    working_directory: PathBuf,
) -> Result<WorkspaceSnapshot, String> {
    let path = snapshot_store_path()
        .ok_or_else(|| "Cannot determine config directory for workspace storage".to_string())?;

    let snap = WorkspaceSnapshot {
        name: name.to_string(),
        session_key,
        cli_source,
        working_directory,
        saved_at: now_rfc3339(),
    };

    let mut store = load_store(&path);
    store.snapshots.insert(name.to_string(), snap.clone());
    save_store(&path, &store)
        .map_err(|e| format!("Failed to write workspace snapshot: {e}"))?;

    Ok(snap)
}

/// Load a named workspace snapshot. Returns `None` when `name` has no saved
/// snapshot yet.
pub fn load(name: &str) -> Result<Option<WorkspaceSnapshot>, String> {
    let path = snapshot_store_path()
        .ok_or_else(|| "Cannot determine config directory for workspace storage".to_string())?;

    let store = load_store(&path);
    Ok(store.snapshots.get(name).cloned())
}

/// Return all stored snapshot names, sorted alphabetically.
pub fn list() -> Result<Vec<String>, String> {
    let path = snapshot_store_path()
        .ok_or_else(|| "Cannot determine config directory for workspace storage".to_string())?;

    let store = load_store(&path);
    let mut names: Vec<String> = store.snapshots.keys().cloned().collect();
    names.sort();
    Ok(names)
}

/// Delete a named snapshot.  No-op (not an error) if the name does not exist.
pub fn delete(name: &str) -> Result<(), String> {
    let path = snapshot_store_path()
        .ok_or_else(|| "Cannot determine config directory for workspace storage".to_string())?;

    let mut store = load_store(&path);
    store.snapshots.remove(name);
    save_store(&path, &store)
        .map_err(|e| format!("Failed to write workspace snapshot: {e}"))
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::NamedTempFile;

    fn dummy_snap(name: &str) -> WorkspaceSnapshot {
        WorkspaceSnapshot {
            name: name.to_string(),
            session_key: "key-abc".to_string(),
            cli_source: CliSource::Claude,
            working_directory: PathBuf::from("/home/user/project"),
            saved_at: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    fn round_trip_via_path(path: &std::path::PathBuf, name: &str) -> WorkspaceSnapshot {
        let snap = dummy_snap(name);
        let mut store = SnapshotStore::default();
        store.snapshots.insert(name.to_string(), snap.clone());
        save_store(path, &store).unwrap();

        let loaded = load_store(path);
        loaded.snapshots.get(name).unwrap().clone()
    }

    #[test]
    fn round_trip_serialization() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();
        let snap = round_trip_via_path(&path, "default");
        assert_eq!(snap.session_key, "key-abc");
        assert_eq!(snap.cli_source, CliSource::Claude);
        assert_eq!(snap.working_directory, Path::new("/home/user/project"));
    }

    #[test]
    fn multiple_snapshots_stored_independently() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();

        let mut store = SnapshotStore::default();
        for name in ["alpha", "beta", "gamma"] {
            store.snapshots.insert(name.to_string(), dummy_snap(name));
        }
        save_store(&path, &store).unwrap();

        let reloaded = load_store(&path);
        assert_eq!(reloaded.snapshots.len(), 3);
        assert!(reloaded.snapshots.contains_key("alpha"));
        assert!(reloaded.snapshots.contains_key("gamma"));
    }

    #[test]
    fn load_missing_file_returns_empty_store() {
        let path = PathBuf::from("/tmp/__wta_no_such_file_xyz__.json");
        let store = load_store(&path);
        assert!(store.snapshots.is_empty());
    }

    #[test]
    fn overwrite_existing_snapshot() {
        let file = NamedTempFile::new().unwrap();
        let path = file.path().to_path_buf();

        let mut store = SnapshotStore::default();
        store
            .snapshots
            .insert("default".to_string(), dummy_snap("default"));
        save_store(&path, &store).unwrap();

        // Overwrite with a different session key.
        let updated = WorkspaceSnapshot {
            name: "default".to_string(),
            session_key: "key-xyz".to_string(),
            cli_source: CliSource::Gemini,
            working_directory: PathBuf::from("/other"),
            saved_at: "2025-06-01T12:00:00Z".to_string(),
        };
        let mut store2 = load_store(&path);
        store2
            .snapshots
            .insert("default".to_string(), updated.clone());
        save_store(&path, &store2).unwrap();

        let final_store = load_store(&path);
        assert_eq!(
            final_store.snapshots["default"].session_key,
            "key-xyz"
        );
        assert_eq!(
            final_store.snapshots["default"].cli_source,
            CliSource::Gemini
        );
    }

    #[test]
    fn days_to_ymd_epoch() {
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2024-01-01 is 19723 days after 1970-01-01.
        assert_eq!(days_to_ymd(19723), (2024, 1, 1));
    }

    #[test]
    fn now_rfc3339_format() {
        let s = now_rfc3339();
        // Basic shape: YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(s.len(), 20, "unexpected length: {s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[7..8], "-");
        assert_eq!(&s[10..11], "T");
        assert!(s.ends_with('Z'));
    }
}
