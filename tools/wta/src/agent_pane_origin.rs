// tools/wta/src/agent_pane_origin.rs
//
// On-disk index of ACP sessions that WTA created on behalf of an
// Intelligent Terminal agent pane.
//
// Why a sidecar file (instead of ACP `_meta` or CLI-specific rename):
//   * ACP `_meta` reaches the agent but agent CLIs (Copilot/Claude/Gemini)
//     are observed not to persist it. So `_meta` cannot survive a restart.
//   * Agent CLIs each generate their own on-disk titles from conversation
//     content; we don't want to interfere with that.
//   * WTA itself owns the moment when a session is created from an agent
//     pane (it's the side that calls ACP `session/new` with `owner_tab_id`
//     in scope), so recording the fact locally is authoritative.
//
// Format
// ------
// JSONL, one record per ACP `session/new` success, appended atomically by
// the OS (`OpenOptions::append`). Records are intentionally small so the
// file stays compact under heavy use:
//
//     {"v":1,"session_id":"<uuid>","origin":"agent_pane","started_at":"<RFC3339-ish>"}
//
// Duplicates are tolerated. `load_set()` collects session ids into a
// `HashSet` so a session that appears multiple times still resolves to a
// single membership check. Lines that fail to parse are skipped and the
// next line is processed — corruption in one record does not invalidate
// the rest of the file.
//
// Lifetime
// --------
// The file is append-only; it is never read-then-written from this module.
// Old entries become orphans naturally when the corresponding CLI session
// directory is deleted by the user or the agent CLI itself — orphan entries
// in the index are harmless because `history_loader` only consults the
// index when constructing rows for sessions that *still exist on disk*.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::SystemTime;

const INDEX_FILENAME: &str = "agent-pane-sessions.jsonl";
const SCHEMA_VERSION: u32 = 1;

/// Resolve the canonical on-disk location for the index. Returns `None`
/// only if neither `%LOCALAPPDATA%` nor `%APPDATA%` is set, which is
/// extremely unusual on Windows but matches the rest of `runtime_paths`.
pub fn default_index_path() -> Option<PathBuf> {
    crate::runtime_paths::intelligent_terminal_root()
        .map(|root| root.join(INDEX_FILENAME))
}

/// Append an `agent_pane` record for `session_id` to the default index.
///
/// Best-effort: any IO error is logged and discarded. The caller must
/// not depend on the write succeeding — a failed append simply means the
/// next history scan won't badge this session, which is graceful
/// degradation rather than breakage.
pub fn append_default(session_id: &str) {
    let Some(path) = default_index_path() else {
        tracing::warn!(
            target: "agent_pane_origin",
            session_id = %session_id,
            "skipping append: no runtime root available",
        );
        return;
    };
    if let Err(err) = append_to(&path, session_id) {
        tracing::warn!(
            target: "agent_pane_origin",
            session_id = %session_id,
            path = %path.display(),
            error = %err,
            "failed to append origin record",
        );
    }
}

/// Append an `agent_pane` record to a caller-supplied path. Public to
/// support unit tests that exercise round-tripping against a tempdir.
pub fn append_to(path: &std::path::Path, session_id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let record = serde_json::json!({
        "v": SCHEMA_VERSION,
        "session_id": session_id,
        "origin": "agent_pane",
        "started_at": rfc3339_now(),
    });
    writeln!(file, "{}", record)?;
    Ok(())
}

/// Load the default index into a `HashSet<String>` of session ids. Empty
/// set if the file does not exist, cannot be opened, or is empty — never
/// errors out to the caller, which lets `history_loader` proceed even on
/// a fresh install or after a manual delete.
pub fn load_default_set() -> HashSet<String> {
    let Some(path) = default_index_path() else { return HashSet::new() };
    load_set_from(&path)
}

/// Load an index file from `path`. Tolerates missing files and corrupt
/// lines. Public for unit tests.
pub fn load_set_from(path: &std::path::Path) -> HashSet<String> {
    let mut out = HashSet::new();
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return out, // most commonly: file does not exist yet
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        let trimmed = line.trim();
        if trimmed.is_empty() { continue; }
        let parsed: Result<serde_json::Value, _> = serde_json::from_str(trimmed);
        let Ok(value) = parsed else { continue }; // skip corrupt line
        if let Some(id) = value.get("session_id").and_then(|v| v.as_str()) {
            if !id.is_empty() {
                out.insert(id.to_string());
            }
        }
    }
    out
}

fn rfc3339_now() -> String {
    // Tiny RFC3339 emitter — we don't pull in chrono just for this. The
    // exact format is unspecified by callers (the index is for our own
    // consumption); a sortable UTC timestamp is enough for `tail -f`
    // debugging.
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // YYYY-MM-DDTHH:MM:SSZ via simple integer math (UTC). Years 1970-2099
    // suffice for our lifetime.
    let (y, mo, d, h, mi, s) = unix_secs_to_ymdhms(secs);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, mi, s)
}

fn unix_secs_to_ymdhms(secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let h = (rem / 3600) as u32;
    let mi = ((rem % 3600) / 60) as u32;
    let s = (rem % 60) as u32;

    // Days since 1970-01-01 → calendar date (Gregorian).
    let mut year: u32 = 1970;
    let mut days_left = days as i64;
    loop {
        let dy = if is_leap_year(year) { 366 } else { 365 };
        if days_left < dy { break; }
        days_left -= dy;
        year += 1;
    }
    let months: [u32; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut month: u32 = 1;
    for &dm in &months {
        if days_left < dm as i64 { break; }
        days_left -= dm as i64;
        month += 1;
    }
    let day = (days_left as u32) + 1;
    (year, month, day, h, mi, s)
}

fn is_leap_year(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_index_path(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("wta-agent-pane-origin-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}-{}.jsonl", label, std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    #[test]
    fn append_then_load_roundtrip() {
        let path = tmp_index_path("roundtrip");
        append_to(&path, "abc-123").unwrap();
        append_to(&path, "def-456").unwrap();
        let set = load_set_from(&path);
        assert!(set.contains("abc-123"));
        assert!(set.contains("def-456"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn duplicate_appends_collapse_in_set() {
        let path = tmp_index_path("dup");
        append_to(&path, "same-id").unwrap();
        append_to(&path, "same-id").unwrap();
        append_to(&path, "same-id").unwrap();
        let set = load_set_from(&path);
        assert_eq!(set.len(), 1);
        assert!(set.contains("same-id"));
    }

    #[test]
    fn missing_file_yields_empty_set() {
        let path = std::env::temp_dir().join("does-not-exist-9f8d3c2.jsonl");
        let _ = std::fs::remove_file(&path);
        let set = load_set_from(&path);
        assert!(set.is_empty());
    }

    #[test]
    fn corrupt_lines_are_skipped() {
        let path = tmp_index_path("corrupt");
        // Pre-seed with garbage + a valid record + more garbage.
        std::fs::write(
            &path,
            "this is not json\n\
             {\"v\":1,\"session_id\":\"good-1\",\"origin\":\"agent_pane\"}\n\
             {malformed\n\
             \n\
             {\"v\":1,\"session_id\":\"good-2\"}\n",
        )
        .unwrap();
        let set = load_set_from(&path);
        assert!(set.contains("good-1"));
        assert!(set.contains("good-2"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn empty_session_id_is_ignored() {
        let path = tmp_index_path("empty-id");
        std::fs::write(
            &path,
            "{\"v\":1,\"session_id\":\"\",\"origin\":\"agent_pane\"}\n\
             {\"v\":1,\"origin\":\"agent_pane\"}\n",
        )
        .unwrap();
        let set = load_set_from(&path);
        assert!(set.is_empty());
    }

    #[test]
    fn rfc3339_now_has_expected_shape() {
        let s = rfc3339_now();
        assert_eq!(s.len(), 20, "expected YYYY-MM-DDTHH:MM:SSZ: {:?}", s);
        assert!(s.ends_with('Z'), "expected trailing Z: {:?}", s);
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
    }

    #[test]
    fn ymdhms_known_dates() {
        // 1779393382 in UTC is 2026-05-21T19:56:22Z; the local time observed
        // in wta-main.log (12:56:22 local) maps to the same UTC instant
        // (PDT = UTC-7 in May).
        let secs = 1_779_393_382;
        let (y, mo, d, h, mi, s) = unix_secs_to_ymdhms(secs);
        assert_eq!((y, mo, d, h, mi, s), (2026, 5, 21, 19, 56, 22));
        // Unix epoch sanity.
        let (y, mo, d, h, mi, s) = unix_secs_to_ymdhms(0);
        assert_eq!((y, mo, d, h, mi, s), (1970, 1, 1, 0, 0, 0));
        // Leap-year boundary: 2024-02-29T00:00:00Z = 1709164800.
        let (y, mo, d, ..) = unix_secs_to_ymdhms(1_709_164_800);
        assert_eq!((y, mo, d), (2024, 2, 29));
    }
}
