//! session_watcher — turn each agent CLI's on-disk session records into the
//! crate's existing [`crate::agent_sessions::SessionEvent`]s, hook-free.
//!
//! The per-CLI `classify_*` functions are the pure, testable core: they take
//! one parsed record plus the session key and return zero or more
//! `SessionEvent`s. The watch loop ([`watch`]) is the thin impure shell that
//! tails files (by byte offset — all four CLIs are append logs) and feeds
//! records through them. Binding a discovered session to its pane lives in
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
    /// Path-encoded session cwd when available (Claude only today; `None`
    /// for Copilot/Codex/Gemini). Consumed by master pane-binding.
    pub cwd: Option<PathBuf>,
    pub event: SessionEvent,
}

/// Per-file progress so we only classify new records.
#[derive(Default)]
pub(crate) struct Progress {
    /// Byte offset for append-only CLIs (all four are append logs).
    offset: u64,
    /// Gemini's canonical session id, resolved once from the file header's
    /// `sessionId` (the filename only carries the first 8 hex chars) and cached
    /// so every emitted event keys to the same id the registry binds. `None`
    /// until first read; falls back to the path-derived key if the header is
    /// unreadable.
    gemini_key: Option<String>,
    /// Set once if this file is a Codex multi-agent subagent fork — its records
    /// are then ignored wholesale (it inherits the parent's history and is not a
    /// user-facing session, so surfacing it would duplicate the parent's row).
    ignored: bool,
}

/// Process one changed file path into emitted events, advancing `progress`.
/// Pure w.r.t. everything except the on-disk file and the passed-in map.
pub fn process_change(path: &Path, progress: &mut HashMap<PathBuf, Progress>) -> Vec<Emitted> {
    let Some(disc) = discover::identify(path) else {
        return Vec::new();
    };
    let entry = progress.entry(path.to_path_buf()).or_default();
    if entry.ignored {
        // A Codex subagent fork detected on a previous read — skip wholesale.
        return Vec::new();
    }
    let mut out = Vec::new();

    match disc.cli {
        CliSource::Gemini => {
            // Gemini's `session-*.jsonl` is an append log (re-verified
            // 2026-06-14): single-message records and `$set` ops are appended at
            // the end; the only rewrites are full `$set.messages` snapshots at
            // start/resume, which `classify_record` skips so a resume can't
            // replay history. Read it by byte offset like the other CLIs.
            //
            // Canonical key = the header `sessionId` (the filename only carries
            // the first 8 hex chars). Resolve + cache it once; fall back to the
            // path-derived key until the header is readable.
            if entry.gemini_key.is_none() {
                entry.gemini_key = read_gemini_session_id(path);
            }
            let key = entry
                .gemini_key
                .clone()
                .unwrap_or_else(|| disc.key.clone());

            let from = entry.offset;
            let Ok((text, len)) = read_appended(path, from) else {
                return out;
            };
            if len < from {
                entry.offset = len;
                return out;
            }
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
                for event in classify_gemini::classify_record(&val, &key) {
                    out.push(Emitted {
                        cli: CliSource::Gemini,
                        key: key.clone(),
                        cwd: disc.cwd.clone(),
                        event,
                    });
                }
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
                // A Codex multi-agent subagent fork carries `source.subagent` in
                // its session_meta (always the first record). Mark the file
                // ignored and drop everything: it inherits the parent's history,
                // so tracking it would duplicate the parent's row.
                if matches!(disc.cli, CliSource::Codex)
                    && crate::history_loader::codex_record_is_subagent_meta(&val)
                {
                    entry.ignored = true;
                    return Vec::new();
                }
                let events = match disc.cli {
                    CliSource::Copilot => classify_copilot::classify(&val, &disc.key),
                    CliSource::Claude => classify_claude::classify(&val, &disc.key),
                    CliSource::Codex => classify_codex::classify(&val, &disc.key),
                    _ => Vec::new(),
                };
                for event in events {
                    out.push(Emitted { cli: disc.cli.clone(), key: disc.key.clone(), cwd: disc.cwd.clone(), event });
                }
            }
        }
    }
    out
}

/// The four watched roots under the user profile. Empty when `USERPROFILE` is
/// unset: returning relative `.copilot/...` paths would make the watcher
/// observe directories under the process CWD, so we disable the watcher cleanly
/// (watch nothing) instead.
pub fn watched_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) else {
        return Vec::new();
    };
    vec![
        home.join(".copilot").join("session-state"),
        home.join(".claude").join("projects"),
        home.join(".codex").join("sessions"),
        home.join(".gemini").join("tmp"),
    ]
}

use std::sync::mpsc::Sender;

/// Seed per-file progress to each existing session file's current end, so the
/// watcher only processes content appended *after* it starts. Without this, the
/// first `notify` event for a preexisting historical file (which the OS can
/// deliver spuriously — e.g. an indexer/AV touch, or a delayed
/// ReadDirectoryChangesW batch) would make `process_change` replay that file's
/// entire record stream from offset 0. Each replayed record revives its
/// historical Class-B session and re-broadcasts `sessions/changed`, flooding
/// master with thousands of redundant notifications and stalling live updates.
///
/// Files created *after* the watcher starts are not seeded (not present here),
/// so their first sighting is still read from offset 0 — correctly catching a
/// new session's opening `session_meta` / `task_started` records.
pub(crate) fn seed_existing_progress_in(
    roots: &[PathBuf],
    progress: &mut HashMap<PathBuf, Progress>,
) {
    for root in roots {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                match entry.file_type() {
                    Ok(ft) if ft.is_dir() => stack.push(path),
                    Ok(_) => {
                        let Some(_disc) = discover::identify(&path) else {
                            continue;
                        };
                        // All four CLIs are append logs — seed each file's
                        // progress to its current end so the watcher only
                        // processes content appended after it starts.
                        let prog = Progress {
                            offset: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
                            ..Default::default()
                        };
                        progress.insert(path, prog);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Gemini's canonical session id, read from the file header's `sessionId`
/// field (the first line). `None` on any read/parse failure or if the header
/// lacks the field. Reads only the first line, not the whole (often large) file.
fn read_gemini_session_id(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let file = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(file).read_line(&mut first).ok()?;
    let val: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    val.get("sessionId")
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

/// Spawn a blocking `notify` watcher over the four roots. Each emitted event
/// is sent on `tx`. Runs until `tx` is dropped or the watcher errors.
///
/// Recursive mode is required: session files live several levels below each
/// root (e.g. `.codex/sessions/YYYY/MM/DD/...`).
///
/// Purely event-driven: a `notify` event for a file runs the incremental
/// [`process_change`]. The watcher is a hookless **fallback** producer, so we
/// deliberately do NOT add a periodic catch-up sweep — `notify`
/// (ReadDirectoryChangesW) can coalesce/drop the event for a turn's final write
/// (e.g. Codex `task_complete`), which may briefly leave a fallback row on its
/// last status until the next file write; that imperfection is acceptable for a
/// fallback, and Ctrl+C cleanup is handled by master's pid-liveness poll.
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
    // Skip every record that already existed when we started watching — only
    // track genuinely new activity. Startup-only (not polling); prevents a
    // spurious notify on a preexisting file from replaying its whole history
    // and flooding master with revive broadcasts. See `seed_existing_progress_in`.
    seed_existing_progress_in(&watched_roots(), &mut progress);

    for res in raw_rx {
        let event = match res {
            Ok(event) => event,
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
            b"{\"type\":\"tool.execution_start\",\"data\":{\"toolName\":\"bash\"}}\n{\"type\":\"assistant.turn",
        )
        .unwrap();
        let mut progress = std::collections::HashMap::new();
        let first = process_change(&path, &mut progress);
        assert_eq!(first.len(), 1, "only the complete line should classify");
        assert!(matches!(first[0].event, SessionEvent::ToolStarting { .. }));
        // Complete the partial record (turn_end → ToolCompleted under the
        // turn-based Copilot model).
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"_end\",\"data\":{\"turnId\":\"0\"}}\n")
            .unwrap();
        let second = process_change(&path, &mut progress);
        assert_eq!(second.len(), 1, "the completed record must now classify (not be lost)");
        assert!(matches!(second[0].event, SessionEvent::ToolCompleted { .. }));
    }

    #[test]
    fn seed_skips_preexisting_history_then_tracks_new_appends() {
        // A preexisting Codex rollout (history) must be seeded to EOF so it is
        // NOT replayed from offset 0 — the bug that flooded master with revive
        // broadcasts. New content appended after seeding is still tracked.
        let root = std::env::temp_dir().join(format!("wta-seed-{}", std::process::id()));
        let day = root.join("2026").join("06").join("10");
        std::fs::create_dir_all(&day).unwrap();
        let path =
            day.join("rollout-2026-06-10T00-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n\
              {\"type\":\"response_item\",\"payload\":{\"type\":\"function_call\",\"name\":\"shell\"}}\n",
        )
        .unwrap();

        let mut progress = HashMap::new();
        seed_existing_progress_in(&[root.clone()], &mut progress);

        // History is skipped — no replay on the first change.
        let replay = process_change(&path, &mut progress);
        assert!(
            replay.is_empty(),
            "seeded historical file must not replay, got {:?}",
            replay
        );

        // A genuinely new appended record IS processed.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n")
            .unwrap();
        let fresh = process_change(&path, &mut progress);
        assert_eq!(fresh.len(), 1, "new appended record must be classified");
        assert!(matches!(fresh[0].event, SessionEvent::ToolCompleted { .. }));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_does_not_skip_files_created_after_start() {
        // A file absent at seed time (new session created after the watcher
        // started) is not seeded, so it's read in full on first sight.
        let root = std::env::temp_dir().join(format!("wta-seed-new-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let mut progress = HashMap::new();
        seed_existing_progress_in(&[root.clone()], &mut progress); // empty root

        let path =
            root.join("rollout-2026-06-10T00-00-00-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();
        let out = process_change(&path, &mut progress);
        assert_eq!(out.len(), 1, "a new file must be read from offset 0");
        assert!(matches!(out[0].event, SessionEvent::ToolStarting { .. }));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn process_change_ignores_codex_subagent_rollout() {
        // Codex's multi_agent_v1/spawn_agent forks a child thread with its own
        // rollout (source.subagent) that inherits the parent's history — it must
        // never surface as its own (duplicate) row.
        let root = std::env::temp_dir().join(format!("wta-subagent-{}", std::process::id()));
        let dir = root.join("2026").join("06").join("10");
        std::fs::create_dir_all(&dir).unwrap();
        let path =
            dir.join("rollout-2026-06-10T13-15-12-99999999-2222-3333-4444-555555555555.jsonl");
        std::fs::write(
            &path,
            b"{\"type\":\"session_meta\",\"payload\":{\"id\":\"99999999-2222-3333-4444-555555555555\",\"forked_from_id\":\"p\",\"source\":{\"subagent\":{\"thread_spawn\":{\"depth\":1}}}}}\n\
              {\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();

        let mut progress = HashMap::new();
        let out = process_change(&path, &mut progress);
        assert!(out.is_empty(), "subagent rollout must emit nothing, got {:?}", out);

        // Even after the subagent does work, it stays ignored.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n")
            .unwrap();
        assert!(
            process_change(&path, &mut progress).is_empty(),
            "a file flagged as a subagent stays ignored on later reads"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
