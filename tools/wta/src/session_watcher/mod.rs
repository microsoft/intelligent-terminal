//! session_watcher — turn each agent CLI's on-disk session records into the
//! crate's existing [`crate::agent_sessions::SessionEvent`]s, hook-free.
//!
//! The per-CLI `classify_*` functions are the pure, testable core: they take
//! one parsed record (or, for Gemini, the rewritten snapshot) plus the
//! session key and return zero or more `SessionEvent`s. The watch loop
//! ([`watch`]) is the thin impure shell that tails files and feeds records
//! through them. Binding a discovered session to its pane lives in
//! [`bind`]; path → identity in [`discover`].

pub mod bind;
pub mod classify_claude;
pub mod classify_codex;
pub mod classify_copilot;
pub mod classify_gemini;
pub mod discover;

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Read the bytes appended to `path` since byte offset `from`, returning the
/// decoded text and the new end offset. Used for the append-only CLIs.
pub fn read_appended(path: &Path, from: u64) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if len <= from {
        return Ok((String::new(), len));
    }
    file.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity((len - from) as usize);
    file.take(len - from).read_to_end(&mut buf)?;
    Ok((String::from_utf8_lossy(&buf).into_owned(), len))
}

use crate::agent_sessions::{CliSource, SessionEvent};

/// One emitted event with its routing identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emitted {
    pub cli: CliSource,
    pub key: String,
    pub event: SessionEvent,
}

/// Per-file progress so we only classify new records.
#[derive(Default)]
pub(crate) struct Progress {
    /// Byte offset for append-only CLIs.
    offset: u64,
    /// Message count for Gemini's snapshot model.
    gemini_msgs: usize,
}

/// Process one changed file path into emitted events, advancing `progress`.
/// Pure w.r.t. everything except the on-disk file and the passed-in map.
pub fn process_change(path: &Path, progress: &mut HashMap<PathBuf, Progress>) -> Vec<Emitted> {
    let Some(disc) = discover::identify(path) else {
        return Vec::new();
    };
    let entry = progress.entry(path.to_path_buf()).or_default();
    let mut out = Vec::new();

    match disc.cli {
        CliSource::Gemini => {
            // Reparse the whole file; take the last non-empty snapshot line.
            let Ok(text) = std::fs::read_to_string(path) else {
                return out;
            };
            // Canonical key = header `sessionId` (the filename only carries the
            // first 8 hex chars). Fall back to the path-derived key if absent.
            let key = text
                .lines()
                .next()
                .and_then(|l| serde_json::from_str::<serde_json::Value>(l).ok())
                .and_then(|v| v.get("sessionId").and_then(|s| s.as_str()).map(str::to_string))
                .unwrap_or_else(|| disc.key.clone());
            let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) else {
                return out;
            };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(last) else {
                return out;
            };
            let (events, new_len) =
                classify_gemini::classify_snapshot(&val, &key, entry.gemini_msgs);
            entry.gemini_msgs = new_len;
            for event in events {
                out.push(Emitted { cli: disc.cli.clone(), key: key.clone(), event });
            }
        }
        _ => {
            let from = entry.offset;
            let Ok((text, len)) = read_appended(path, from) else {
                return out;
            };
            if len < from {
                // File shrank/rotated — resync to the new end, drop nothing
                // further this tick.
                entry.offset = len;
                return out;
            }
            // Only consume through the last newline; a trailing partial line is
            // a record still being written — leave its bytes for the next tick.
            let consumed = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
            entry.offset = from + consumed as u64;
            for line in text[..consumed].lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                let events = match disc.cli {
                    CliSource::Copilot => classify_copilot::classify(&val, &disc.key),
                    CliSource::Claude => classify_claude::classify(&val, &disc.key),
                    CliSource::Codex => classify_codex::classify(&val, &disc.key),
                    _ => Vec::new(),
                };
                for event in events {
                    out.push(Emitted { cli: disc.cli.clone(), key: disc.key.clone(), event });
                }
            }
        }
    }
    out
}

/// The four watched roots under the user profile.
pub fn watched_roots() -> Vec<PathBuf> {
    let home = std::env::var("USERPROFILE")
        .map(PathBuf::from)
        .unwrap_or_default();
    vec![
        home.join(".copilot").join("session-state"),
        home.join(".claude").join("projects"),
        home.join(".codex").join("sessions"),
        home.join(".gemini").join("tmp"),
    ]
}

use std::sync::mpsc::Sender;

/// Spawn a blocking `notify` watcher over the four roots. Each emitted event
/// is sent on `tx`. Runs until `tx` is dropped or the watcher errors.
///
/// Recursive mode is required: session files live several levels below each
/// root (e.g. `.codex/sessions/YYYY/MM/DD/...`).
pub fn watch(tx: Sender<Emitted>) -> notify::Result<()> {
    use notify::{RecursiveMode, Watcher};

    let (raw_tx, raw_rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = raw_tx.send(res);
    })?;
    for root in watched_roots() {
        // A missing root is fine (the user may not have that CLI) — log + skip.
        if root.exists() {
            if let Err(err) = watcher.watch(&root, RecursiveMode::Recursive) {
                tracing::warn!(
                    target: "session_watcher",
                    root = %root.display(),
                    error = %err,
                    "watch failed"
                );
            }
        }
    }

    let mut progress: HashMap<PathBuf, Progress> = HashMap::new();
    for res in raw_rx {
        let event = match res {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(target: "session_watcher", error = %err, "notify error");
                continue;
            }
        };
        for path in event.paths {
            for emitted in process_change(&path, &mut progress) {
                if tx.send(emitted).is_err() {
                    return Ok(()); // receiver gone
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn read_appended_returns_only_new_bytes() {
        let dir = std::env::temp_dir().join(format!("wta-watch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.jsonl");
        std::fs::write(&path, b"line1\n").unwrap();
        let (first, off1) = read_appended(&path, 0).unwrap();
        assert_eq!(first, "line1\n");
        assert_eq!(off1, 6);
        // Append more, read only the delta.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"line2\n")
            .unwrap();
        let (second, off2) = read_appended(&path, off1).unwrap();
        assert_eq!(second, "line2\n");
        assert_eq!(off2, 12);
    }

    #[test]
    fn process_change_emits_copilot_events_incrementally() {
        let dir = std::env::temp_dir()
            .join(format!("wta-pc-{}", std::process::id()))
            .join("session-state")
            .join("sess-9");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n",
        )
        .unwrap();
        let mut progress = HashMap::new();
        let first = process_change(&path, &mut progress);
        assert_eq!(first.len(), 1);
        assert!(matches!(first[0].event, SessionEvent::ToolStarting { .. }));
        // No new bytes -> no duplicate events.
        let second = process_change(&path, &mut progress);
        assert!(second.is_empty());
    }

    #[test]
    fn process_change_does_not_lose_a_partial_line() {
        let dir = std::env::temp_dir()
            .join(format!("wta-partial-{}", std::process::id()))
            .join("session-state")
            .join("sess-partial");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        // One complete record + a half-written second record (no newline yet).
        std::fs::write(
            &path,
            b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n{\"type\":\"tool.execu",
        )
        .unwrap();
        let mut progress = std::collections::HashMap::new();
        let first = process_change(&path, &mut progress);
        assert_eq!(first.len(), 1, "only the complete line should classify");
        assert!(matches!(first[0].event, SessionEvent::ToolStarting { .. }));
        // Complete the partial record.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"tion_complete\",\"data\":{\"success\":true}}\n")
            .unwrap();
        let second = process_change(&path, &mut progress);
        assert_eq!(second.len(), 1, "the completed record must now classify (not be lost)");
        assert!(matches!(second[0].event, SessionEvent::ToolCompleted { .. }));
    }
}
